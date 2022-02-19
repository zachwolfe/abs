use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::process::Command;
use std::ffi::{OsStr, OsString};
use std::os::windows::prelude::*;
use std::collections::{HashMap, HashSet};
use std::array::IntoIter;
use std::iter::once;
use std::sync::{Arc, Mutex};

use futures::future::join_all;

use indicatif::{ProgressBar, ProgressStyle, WeakProgressBar};

use serde::{Serialize, Deserialize};

use crate::proj_config::{Platform, Os, ProjectConfig, OutputType};
use crate::cmd_options::BuildOptions;
use crate::canonicalize;
use crate::toolchain_paths::ToolchainPaths;
use crate::println_above_progress_bar_if_visible;
use crate::task::{CxxTask, Task, TaskExt};

// TODO: All fields of BuildEnvironment should be made private again after task.rs
// stops depending on being able to access them.
pub struct BuildEnvironment<'a> {
    pub config_path: PathBuf,
    pub manifest_path: Option<PathBuf>,

    pub linker_lib_dependencies: Vec<PathBuf>,
    
    pub toolchain_paths: &'a ToolchainPaths,
    pub config: &'a ProjectConfig,
    pub build_options: &'a BuildOptions,
    pub definitions: &'a [(&'a str, &'a str)],
    pub project_path: PathBuf,
    pub artifact_path: PathBuf,
    pub src_dir_path: PathBuf,
    pub assets_dir_path: PathBuf,
    pub objs_path: PathBuf,
    pub src_deps_path: PathBuf,
    pub dependency_headers_path: PathBuf,
    pub warning_cache_path: PathBuf,

    pub file_edit_times: Mutex<HashMap<PathBuf, u64>>,
    pub unique_compiler_output: Arc<Mutex<HashSet<String>>>,
    pub progress_bar: Mutex<WeakProgressBar>,
}

#[derive(Debug)]
pub enum BuildError {
    NoSrcDirectory,
    CantReadSrcDirectory,
    DiscoverSrcDepsError,
    CompilerError,
    LinkerError,

    IoError(io::Error),
}

impl From<io::Error> for BuildError {
    fn from(err: io::Error) -> Self {
        BuildError::IoError(err)
    }
}

#[derive(Default)]
pub struct SrcPaths {
    pub root: PathBuf,
    pub src_paths: Vec<PathBuf>,
    pub header_paths: Vec<PathBuf>,
    pub children: Vec<SrcPaths>,
}

impl SrcPaths {
    pub fn from_root(root: impl Into<PathBuf>) -> io::Result<SrcPaths> {
        fn src_paths(root: PathBuf, entries: impl IntoIterator<Item=io::Result<fs::DirEntry>>) -> io::Result<SrcPaths> {
            let mut paths = SrcPaths::default();
            paths.root = root;
            for entry in entries {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_file() {
                    let path = entry.path();
                    if let Some(extension) = path.extension().and_then(OsStr::to_str) {
                        match extension {
                            "cpp" | "cxx" | "cc"   => paths.src_paths.push(path),
                            "h" | "hpp" => paths.header_paths.push(path),
                            _ => {},
                        }
                    }
                } else if file_type.is_dir() {
                    let path = entry.path();
                    let entries = fs::read_dir(&path)?;
                    let child = src_paths(path, entries)?;
                    paths.children.push(child);
                }
            }
            Ok(paths)
        }
        
        let root = root.into();
        let entries = fs::read_dir(&root)?;
        let src_paths = src_paths(root, entries)?;
        Ok(src_paths)
    }
}

fn cmd_flag(flag: impl AsRef<OsStr>, argument: impl AsRef<OsStr>) -> OsString {
    let mut string = flag.as_ref().to_owned();
    string.push(argument);
    string
}

#[derive(Clone, Default)]
pub struct DependencyBuilder {
    dependencies: Vec<PathBuf>,
}

impl DependencyBuilder {
    pub fn file(mut self, path: impl Into<PathBuf>) -> Self {
        self.dependencies.push(path.into());
        self
    }

    pub fn files(mut self, files: impl IntoIterator<Item=impl Into<PathBuf>>) -> Self {
        self.dependencies.extend(files.into_iter().map(|path| path.into()));
        self
    }

    pub fn build(self) -> Vec<PathBuf> { self.dependencies }
}

fn run_cmd(cmd: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>, bin_paths: &[PathBuf], error: BuildError) -> Result<(), BuildError> {
    let mut path = OsString::from("%PATH%");
    for i in 0..bin_paths.len() {
        path.push(";");
        path.push(bin_paths[i].as_os_str());
    }
    let code = Command::new(cmd)
        .args(args)
        .env("PATH", path)
        .spawn()?
        .wait()?;

    if code.success() {
        Ok(())
    } else {
        Err(error)
    }
}

fn get_ps_args(cmd: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>) -> impl IntoIterator<Item=impl AsRef<OsStr>> {
    let mut command = cmd.as_ref().to_owned();
    for arg in args {
        command.push(" ");
        let arg: &OsStr = arg.as_ref();
        let lossy_str = arg.to_string_lossy();
        // The first condition here is a hack to support arrays
        if lossy_str.chars().next() != Some('@') && lossy_str.contains(char::is_whitespace) {
            command.push("\"");
            command.push(arg);
            command.push("\"");
        } else {
            command.push(arg);
        }
    }
    IntoIter::new(["-command".into(), command])
}

#[allow(unused)]
fn run_ps_cmd(cmd: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>, error: BuildError) -> Result<(), BuildError> {
    run_cmd("powershell", get_ps_args(cmd, args), &[], error)
}

#[derive(Copy, Clone)]
pub enum PchOption {
    GeneratePch,
    UsePch,
    NoPch,
}

#[derive(Default, Serialize, Deserialize)]
pub struct WarningCache {
    pub warnings: Vec<String>,
}

impl<'a> BuildEnvironment<'a> {
    pub fn new(
        config: &'a ProjectConfig,
        config_path: impl Into<PathBuf>,
        build_options: &'a BuildOptions,
        toolchain_paths: &'a ToolchainPaths,
        definitions: &'a [(&'a str, &'a str)],
        artifact_path: impl Into<PathBuf>,
    ) -> Result<Self, BuildError> {
        let host = Platform::host();
        let config_path = config_path.into();
        let mut project_path = config_path.clone();
        project_path.pop();
        let manifest_path = project_path.join("windows_manifest.xml");
        let has_manifest = manifest_path.exists();
        let linker_lib_dependencies = match host.os() {
            Os::Windows => {
                let mut dependencies = DependencyBuilder::default();
                // TODO: Speed!!!
                for lib in &config.link_libraries {
                    for lib_path in &toolchain_paths.lib_paths {
                        for entry in fs::read_dir(lib_path).unwrap() {
                            let entry = entry.unwrap();
                            if entry.file_type().unwrap().is_file() && entry.file_name().to_str().map(str::to_lowercase) == Some(lib.to_lowercase()) {
                                dependencies = dependencies.file(entry.path());
                            }
                        }
                    }
                }
                dependencies.build()
            }
        };
        let artifact_path = artifact_path.into();
        let objs_path = artifact_path.join("obj");
        let src_deps_path = artifact_path.join("src_deps");
        let dependency_headers_path = artifact_path.join("dependency_headers");
        let warning_cache_path = artifact_path.join("warning_cache");
        fs::create_dir_all(&objs_path)?;
        fs::create_dir_all(&src_deps_path)?;
        fs::create_dir_all(&dependency_headers_path)?;
        fs::create_dir_all(&warning_cache_path)?;

        let src_dir_path = project_path.join("src");
        let assets_dir_path = project_path.join("assets");

        Ok(BuildEnvironment {
            config_path,
            manifest_path: if has_manifest {
                Some(manifest_path)
            } else {
                None
            },

            linker_lib_dependencies,

            toolchain_paths,
            config,
            build_options,
            definitions,
            project_path,
            artifact_path,
            src_dir_path,
            assets_dir_path,
            objs_path,
            src_deps_path,
            dependency_headers_path,
            warning_cache_path,

            file_edit_times: Default::default(),
            unique_compiler_output: Default::default(),
            progress_bar: Mutex::new(ProgressBar::new(0).downgrade()),
        })
    }

    fn edit_time(&self, path: impl AsRef<Path>, fallback: u64) -> io::Result<u64> {
        let path = path.as_ref();
        let mut edit_times = self.file_edit_times.lock().unwrap();
        if let Some(&edit_time) = edit_times.get(path) {
            Ok(edit_time)
        } else {
            let time = match fs::metadata(path) {
                Ok(metadata) => metadata.last_write_time(),
                Err(err) if matches!(err.kind(), io::ErrorKind::NotFound) => fallback,
                Err(err) => return Err(err),
            };
            edit_times.insert(path.to_owned(), time);
            Ok(time)
        }
    }

    pub fn fail(&self, error: BuildError) -> ! {
        print!("Build failed: ");
        match error {
            BuildError::NoSrcDirectory => println!("src directory does not exist."),
            BuildError::CantReadSrcDirectory => println!("unable to read src directory."),
            BuildError::DiscoverSrcDepsError => println!("unable to discover source dependencies."),
            BuildError::CompilerError => println!("unable to compile."),
            BuildError::LinkerError => println!("unable to link."),

            BuildError::IoError(io_error) => println!("there was an io error: {:?}.", io_error.kind()),
        }
        std::process::exit(1);
    }

    fn should_build_artifacts_impl(
        &self,
        dependency_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        artifact_paths: impl IntoIterator<Item=impl AsRef<Path>> + Clone,
        mut filter: impl FnMut(&Path) -> bool,
    ) -> io::Result<bool> {
        // If the config file has changed, I want to rebuild the whole project, so unconditionally add it
        // as a dependency.
        let config_path = self.config_path.clone();
        let config_edit_time = self.edit_time(config_path, u64::MAX);
        // TODO: shouldn't really be necessary to collect in a Vec here.
        let dependencies: Result<Vec<_>, _> = dependency_paths.into_iter()
            .map(|path| self.edit_time(path, u64::MAX))
            .chain(once(config_edit_time))
            .collect();
        let dependencies = dependencies?;
        let newest_dependency = dependencies.into_iter().max().unwrap_or(0u64);

        let artifacts: Result<Vec<_>, _> = artifact_paths.clone().into_iter()
            .filter(|path|
                filter(path.as_ref())
            )
            .map(|path| self.edit_time(path, 0u64))
            .collect();
        let artifacts = artifacts?;
        let oldest_artifact = artifacts.into_iter().min().unwrap_or(0u64);

        let should_build_artifacts = newest_dependency > oldest_artifact;

        // Invalidate edit times of all artifact paths
        if should_build_artifacts {
            let mut file_edit_times = self.file_edit_times.lock().unwrap();
            for artifact in artifact_paths {
                file_edit_times.remove(artifact.as_ref());
            }
        }

        Ok(should_build_artifacts)
    }

    pub fn should_build_artifact(&self, dependency_paths: impl IntoIterator<Item=impl AsRef<Path>>, artifact_path: impl AsRef<Path> + Clone) -> io::Result<bool> {
        self.should_build_artifacts_impl(dependency_paths, IntoIter::new([artifact_path]), |_| true)
    }

    #[allow(unused)]
    fn should_build_artifacts(&self, dependency_paths: impl IntoIterator<Item=impl AsRef<Path>>, artifact_path: impl AsRef<Path>, extensions: impl IntoIterator<Item=impl AsRef<OsStr>> + Clone) -> io::Result<bool> {
        let artifact_path = artifact_path.as_ref();
        if !artifact_path.exists() { return Ok(true); }
        let artifact_paths: Result<Vec<_>, _> = fs::read_dir(artifact_path)?.map(|entry| entry.map(|entry| entry.path())).collect();
        
        self.should_build_artifacts_impl(
            dependency_paths,
            artifact_paths?,
            |artifact| extensions.clone().into_iter().any(|desired| artifact.extension() == Some(desired.as_ref()))
        )
    }

    fn copy_headers(&self, paths: &SrcPaths, dependency_name: &OsStr, root: &Path, dest_headers_path: &Path) -> Result<(), BuildError> {
        for header_path in &paths.header_paths {
            let copied_header_path = self.get_artifact_path_relative_to(header_path, root, &dest_headers_path);
            fs::create_dir_all(copied_header_path.parent().unwrap())?;
            fs::copy(header_path, &copied_header_path)?;
        }
        for child in &paths.children {
            self.copy_headers(child, dependency_name, root, dest_headers_path)?;
        }
        Ok(())
    }

    pub async fn build(&mut self) -> Result<bool, BuildError> {
        let paths = match SrcPaths::from_root(&self.src_dir_path) {
            Ok(paths) => paths,
            Err(error) => {
                if let io::ErrorKind::NotFound = error.kind() {
                    return Err(BuildError::NoSrcDirectory);
                } else {
                    return Err(BuildError::CantReadSrcDirectory);
                }
            }
        };
        for path in &self.config.dependencies {
            let path = self.project_path.join(path);
            let path = crate::canonicalize(path).unwrap();
            // TODO: use project name instead of the file name
            let project_name = path.file_name().unwrap();
            let path = path.join("src");
            let paths = SrcPaths::from_root(&path).unwrap();
            let dest_headers_path = self.dependency_headers_path.join(project_name);
            // Don't allow a project to include headers that were deleted from the original dependency
            // project. Ignore any errors, because the destination directory may not exist yet, and
            // because this is not a critical operation.
            let _ = fs::remove_dir_all(&dest_headers_path);
            self.copy_headers(&paths, project_name, &paths.root, &dest_headers_path)?;
        }
        let pch = paths.src_paths.iter().any(|path| path.file_name() == Some(OsStr::new("pch.cpp")));
        if pch {
            let pch_path = self.src_dir_path.join("pch.cpp");
            let task = CxxTask::compile(&pch_path, PchOption::GeneratePch);
            if task.previous_valid_run(self)?.is_none() {
                let progress_bar = ProgressBar::new_spinner()
                    .with_message("Generating pre-compiled header");
                progress_bar.enable_steady_tick(50);
                task.run(self).await?;
            }
        };
        let mut obj_paths = Vec::new();
        self.compile_sources(&paths, &mut obj_paths, pch).await?;

        let extension = match self.config.output_type {
            OutputType::ConsoleApp | OutputType::GuiApp => "exe",
            OutputType::DynamicLibrary => "dll",
            OutputType::StaticLibrary => "lib",
        };
        let product_name = format!("{}.{}", self.config.name, extension);
        let pdb_name = format!("{}.pdb", self.config.name);
        let product_path = self.artifact_path.join(&product_name);
        let pdb_path = self.artifact_path.join(&pdb_name);

        let dependencies: Vec<_> = obj_paths.clone().iter().cloned()
            .chain(self.linker_lib_dependencies.iter().cloned())
            .chain(self.manifest_path.iter().cloned())
            .collect();

        super::kill_debugger();
        super::kill_process(&product_name);

        // File locks may continue to be held on the product for some time after it is
        // terminated/unloaded, causing linking to fail. So, while the exit code is 1, keep trying
        // to kill.
        //
        // This is kind of a hack, but it seems to work well enough.
        while super::kill_debugger() == Some(1) {}
        while super::kill_process(&product_name) == Some(1) {}
            
        let should_relink = self.should_build_artifact(&dependencies, &product_path)?;
        let built_artifact = if should_relink {
            self.link(&product_path, obj_paths)?
        } else {
            true
        };

        let mut package_file_paths = vec![product_path, pdb_path];
        if self.assets_dir_path.exists() && fs::metadata(&self.assets_dir_path)?.is_dir() {
            if matches!(self.config.output_type, OutputType::StaticLibrary) {
                println_above_progress_bar_if_visible!(self.progress_bar.lock().unwrap(), "Warning: {} has an assets directory, which is unsupported in static library projects. It will be ignored.", self.config.name);
                if let Ok(canon) = canonicalize(&self.assets_dir_path) {
                    println_above_progress_bar_if_visible!(self.progress_bar.lock().unwrap(), "    assets directory found at path: \"{}\"\n", canon.as_os_str().to_string_lossy());
                }
            }
            package_file_paths.push(self.assets_dir_path.clone());
        }
        Ok(built_artifact)
    }

    /// Goes from a src file path to an artifact path relative to output_dir_path
    /// (e.g., src/hello/world.cpp -> abs/debug/obj/hello/world.obj)
    pub fn get_artifact_path(&self, src_path: impl AsRef<Path>, output_dir_path: impl AsRef<Path>, extension: impl AsRef<OsStr>) -> PathBuf {
        let mut path = self.get_artifact_path_relative_to(src_path, &self.src_dir_path, output_dir_path);
        let succ = path.set_extension(extension);
        assert!(succ);
        path
    }

    fn get_artifact_path_relative_to(&self, src_path: impl AsRef<Path>, relative_to: impl AsRef<Path>, output_dir_path: impl AsRef<Path>) -> PathBuf {
        let mut path = output_dir_path.as_ref().to_owned();
        // Isolate the src file name
        let src_path = src_path.as_ref().strip_prefix(relative_to)
            .expect("path to a src file must have src directory as a prefix");
        path.push(src_path);
        path
    }

    pub fn assemble_sources_to_rebuild<'b>(&self, paths: &'b SrcPaths, obj_paths: &mut Vec<PathBuf>, cached_warnings: &mut Vec<String>, pch: PchOption, sources: &mut Vec<PathBuf>) -> Result<(), BuildError> {
        fs::create_dir_all(&paths.root).unwrap();
        for path in paths.src_paths.iter() {
            let obj_path = self.get_artifact_path(path, &self.objs_path, "obj");
            let warning_cache_path = self.get_artifact_path(path, &self.warning_cache_path, "warnings");

            obj_paths.push(obj_path.clone());

            let dependencies = self.discover_src_deps(path)?.map(|dependencies| {
                DependencyBuilder::default()
                    .file(path)
                    .files(dependencies)
                    .build()
            });

            let task = CxxTask::compile(path, pch);
            if task.previous_valid_run(self)?.is_none() {
                sources.push(path.clone());
            } else {
                let warning_cache_out_of_date = if let Some(dependencies) = &dependencies {
                    self.should_build_artifact(dependencies, &warning_cache_path)?
                } else {
                    true
                };
                if !warning_cache_out_of_date {
                    if let Ok(warning_cache) = fs::read_to_string(warning_cache_path) {
                        if let Ok(warning_cache) = serde_json::from_str::<WarningCache>(&warning_cache) {
                            for warning in warning_cache.warnings {
                                cached_warnings.push(warning);
                            }
                        }
                    }
                }
            }
        }

        for child in &paths.children {
            self.assemble_sources_to_rebuild(child, obj_paths, cached_warnings, pch, sources)?;
        }

        Ok(())
    }
    pub async fn compile_sources<'b>(
        &self,
        paths: &'b SrcPaths,
        obj_paths: &mut Vec<PathBuf>,
        pch: bool,
    ) -> Result<(), BuildError> {
        let mut jobs = Vec::new();
        let mut cached_warnings = Vec::new();
        let pch_option = if pch { PchOption::UsePch } else { PchOption::NoPch };
        self.assemble_sources_to_rebuild(paths, obj_paths, &mut cached_warnings, pch_option, &mut jobs)?;
        let mut job_futures = Vec::new();
        let mut progress_bar: Option<ProgressBar> = None;
        for job in jobs {
            if let Some(progress_bar) = &progress_bar {
                progress_bar.inc_length(1);
                progress_bar.tick();
            } else {
                let pb = ProgressBar::new(1)
                    .with_style(
                        ProgressStyle::default_bar().template("{bar} Compiling source files | {pos}/{len}")
                    );
                *self.progress_bar.lock().unwrap() = pb.downgrade();

                // TODO: this is a hack and shouldn't be necessary. the only time the
                // progress bar updates is when something changes, and that should already
                // trigger an update without this line. But it doesn't for some reason...
                pb.enable_steady_tick(30);
                progress_bar = Some(pb);
            };
            let obj_path = self.get_artifact_path(&job, &self.objs_path, "obj");
            let mut obj_subdir_path = obj_path;
            obj_subdir_path.pop();
            fs::create_dir_all(&obj_subdir_path).unwrap();

            let fut = async move {
                let task = CxxTask::compile(job, pch_option);
                task.run(self).await.map(|_| ())
            };
            job_futures.push(fut);
        }
        let mut res = Ok(());
        let mut succ = 0;
        let mut fail = 0;
        let num_jobs = job_futures.len();
        for job_res in join_all(job_futures).await {
            match job_res {
                Ok(()) => succ += 1,
                Err(err) => {
                    res = Err(err);
                    fail += 1;
                }
            }
        }

        for warning in cached_warnings {
            if self.unique_compiler_output.lock().unwrap().insert(warning.lines().next().unwrap().to_string()) {
                println_above_progress_bar_if_visible!(self.progress_bar.lock().unwrap(), "{}", warning);
            }
        }

        if fail > 0 {
            println_above_progress_bar_if_visible!(self.progress_bar.lock().unwrap(), "Compiled: {}/{} | Failed: {}/{}", succ, num_jobs, fail, num_jobs);
        }
        res
    }

    pub fn discover_src_deps(&self, path: impl AsRef<Path>) -> Result<Option<Vec<PathBuf>>, BuildError> {
        // TODO: Support MSVC's versioning
        #[derive(Deserialize)]
        struct SrcDeps {
            #[serde(rename = "Data")]
            data: SrcDepsData,
        }

        #[derive(Deserialize)]
        struct SrcDepsData {
            #[serde(rename = "Includes")]
            includes: Vec<PathBuf>,
            #[serde(rename = "PCH")]
            pch: Option<PathBuf>,
        }

        let path = path.as_ref();
        let src_deps_json_path = self.get_artifact_path(&path, &self.src_deps_path, "json");
        if self.should_build_artifact([path], &src_deps_json_path)? {
            Ok(None)
        } else {
            let src_deps_file = File::open(&src_deps_json_path)?;
            let src_deps_reader = BufReader::new(src_deps_file);
            let src_deps: SrcDeps = serde_json::from_reader(src_deps_reader)
                .or(Err(BuildError::DiscoverSrcDepsError))?;

            let mut dependencies = DependencyBuilder::default()
                .files(src_deps.data.includes);
            if let Some(pch) = src_deps.data.pch {
                dependencies = dependencies.file(pch);
            }
            
            Ok(Some(dependencies.build()))
        }
    }
    pub fn link(
        &mut self,
        output_path: impl AsRef<Path>,
        obj_paths: impl IntoIterator<Item=impl AsRef<Path>> + Clone,
    ) -> Result<bool, BuildError> {
        let progress_bar = ProgressBar::new_spinner()
            .with_message(format!("Linking {}", output_path.as_ref().to_string_lossy()));
        progress_bar.enable_steady_tick(50);

        let host = Platform::host();
        let output_path = output_path.as_ref();
        let mut args = match host.os() {
            Os::Windows => {
                let mut flags: Vec<OsString> = vec![
                    "/nologo".into(),
                ];
                let output_flag = match self.config.output_type {
                    OutputType::GuiApp => Some("/SUBSYSTEM:WINDOWS"),
                    OutputType::ConsoleApp => Some("/SUBSYSTEM:CONSOLE"),
                    OutputType::DynamicLibrary => Some("/DLL"),
                    OutputType::StaticLibrary => None,
                };
                if let Some(output_flag) = output_flag {
                    flags.push(output_flag.into());
                }
                if !matches!(self.config.output_type, OutputType::StaticLibrary) {
                    flags.push("/manifest:embed".into());
                    flags.push("/debug".into());
                }
                if let Some(manifest_path) = &self.manifest_path {
                    let mut flag = OsString::from("/manifestinput:");
                    flag.push(manifest_path);
                    flags.push(flag);
                    flags.push("/manifestuac:no".into());
                } else {
                    match self.config.output_type {
                        OutputType::GuiApp => {
                            flags.push("/manifestdependency:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0'
                            processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'".into());
                        }
                        OutputType::ConsoleApp | OutputType::DynamicLibrary | OutputType::StaticLibrary => {},
                    }
                }
                for path in &self.toolchain_paths.lib_paths {
                    flags.push(cmd_flag("/LIBPATH:", path));
                }
                flags
            }
        };
        args.push(
            cmd_flag(
                "/out:",
                output_path,
            )
        );
        for path in obj_paths {
            args.push(path.as_ref().as_os_str().to_owned());
        }
        let res = if matches!(self.config.output_type, OutputType::StaticLibrary) {
            run_cmd("lib.exe", &args, &self.toolchain_paths.bin_paths, BuildError::LinkerError)?;
            Ok(output_path.exists())
        } else {
            for path in &self.config.link_libraries {
                args.push(path.into());
            }
            run_cmd("link.exe", &args, &self.toolchain_paths.bin_paths, BuildError::LinkerError)?;
            Ok(true)
        };
        res
    }
}
