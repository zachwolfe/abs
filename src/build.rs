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

use tokio::sync::mpsc;
use tokio::task;
use futures::future::join_all;

use serde::{Serialize, Deserialize};

use crate::proj_config::{Platform, Os, ProjectConfig, OutputType};
use crate::cmd_options::{BuildOptions, CompileMode};
use crate::canonicalize;
use crate::toolchain_paths::ToolchainPaths;
use crate::build_manager::{CompilerOutput, CompileFlags, compile_cxx};

pub struct BuildEnvironment<'a> {
    config_path: PathBuf,
    manifest_path: Option<PathBuf>,

    linker_lib_dependencies: Vec<PathBuf>,
    
    toolchain_paths: &'a ToolchainPaths,
    config: &'a ProjectConfig,
    build_options: &'a BuildOptions,
    definitions: &'a [(&'a str, &'a str)],
    project_path: PathBuf,
    artifact_path: PathBuf,
    src_dir_path: PathBuf,
    assets_dir_path: PathBuf,
    objs_path: PathBuf,
    src_deps_path: PathBuf,
    dependency_headers_path: PathBuf,
    warning_cache_path: PathBuf,

    file_edit_times: HashMap<PathBuf, u64>,
    unique_compiler_output: Arc<Mutex<HashSet<String>>>,
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
struct DependencyBuilder {
    dependencies: Vec<PathBuf>,
}

impl DependencyBuilder {
    fn file(mut self, path: impl Into<PathBuf>) -> Self {
        self.dependencies.push(path.into());
        self
    }

    fn files(mut self, files: impl IntoIterator<Item=impl Into<PathBuf>>) -> Self {
        self.dependencies.extend(files.into_iter().map(|path| path.into()));
        self
    }

    fn build(self) -> Vec<PathBuf> { self.dependencies }
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
enum PchOption {
    GeneratePch,
    UsePch,
    NoPch,
}

#[derive(Default, Serialize, Deserialize)]
struct WarningCache {
    warnings: Vec<String>,
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

            file_edit_times: HashMap::new(),
            unique_compiler_output: Default::default(),
        })
    }

    fn edit_time(&mut self, path: impl AsRef<Path>, fallback: u64) -> io::Result<u64> {
        let path = path.as_ref();
        if let Some(&edit_time) = self.file_edit_times.get(path) {
            Ok(edit_time)
        } else {
            let time = match fs::metadata(path) {
                Ok(metadata) => metadata.last_write_time(),
                Err(err) if matches!(err.kind(), io::ErrorKind::NotFound) => fallback,
                Err(err) => return Err(err),
            };
            self.file_edit_times.insert(path.to_owned(), time);
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
        &mut self,
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
            for artifact in artifact_paths {
                self.file_edit_times.remove(artifact.as_ref());
            }
        }

        Ok(should_build_artifacts)
    }

    fn should_build_artifact(&mut self, dependency_paths: impl IntoIterator<Item=impl AsRef<Path>>, artifact_path: impl AsRef<Path> + Clone) -> io::Result<bool> {
        self.should_build_artifacts_impl(dependency_paths, IntoIter::new([artifact_path]), |_| true)
    }

    #[allow(unused)]
    fn should_build_artifacts(&mut self, dependency_paths: impl IntoIterator<Item=impl AsRef<Path>>, artifact_path: impl AsRef<Path>, extensions: impl IntoIterator<Item=impl AsRef<OsStr>> + Clone) -> io::Result<bool> {
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
            let should_rebuild = if let Some(dependencies) = self.discover_src_deps(&pch_path)? {
                let dependencies = DependencyBuilder::default()
                    .file(&pch_path)
                    .files(dependencies);
                let gen_pch_path = self.get_artifact_path(&pch_path, &self.objs_path, "pch");
                self.should_build_artifact(dependencies.build(), &gen_pch_path)?
            } else {
                true
            };

            if should_rebuild {
                println!("Generating pre-compiled header");
                self.compile(pch_path, &self.objs_path, PchOption::GeneratePch).await?;
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
                println!("Warning: {} has an assets directory, which is unsupported in static library projects. It will be ignored.", self.config.name);
                if let Ok(canon) = canonicalize(&self.assets_dir_path) {
                    println!("    assets directory found at path: \"{}\"\n", canon.as_os_str().to_string_lossy());
                }
            }
            package_file_paths.push(self.assets_dir_path.clone());
        }
        Ok(built_artifact)
    }

    /// Goes from a src file path to an artifact path relative to output_dir_path
    /// (e.g., src/hello/world.cpp -> abs/debug/obj/hello/world.obj)
    fn get_artifact_path(&self, src_path: impl AsRef<Path>, output_dir_path: impl AsRef<Path>, extension: impl AsRef<OsStr>) -> PathBuf {
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

    pub fn assemble_sources_to_rebuild<'b>(&mut self, paths: &'b SrcPaths, obj_paths: &mut Vec<PathBuf>, cached_warnings: &mut Vec<String>, pch: bool, sources: &mut Vec<PathBuf>) -> Result<(), BuildError> {
        fs::create_dir_all(&paths.root).unwrap();
        for path in paths.src_paths.iter() {
            let obj_path = self.get_artifact_path(path, &self.objs_path, "obj");
            let warning_cache_path = self.get_artifact_path(path, &self.warning_cache_path, "warnings");
            obj_paths.push(obj_path.clone());
            let is_pch = path.file_name() == Some(OsStr::new("pch.cpp")) && path.parent() == Some(&self.src_dir_path);
            let dependencies = self.discover_src_deps(path)?.map(|dependencies| {
                DependencyBuilder::default()
                    .file(path)
                    .files(dependencies)
                    .build()
            });
            let should_rebuild = !is_pch && if let Some(dependencies) = &dependencies {
                self.should_build_artifact(dependencies, &obj_path)?
            } else {
                true
            };

            
            if should_rebuild {
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
        &mut self,
        paths: &'b SrcPaths,
        obj_paths: &mut Vec<PathBuf>,
        pch: bool,
    ) -> Result<(), BuildError> {
        let mut jobs = Vec::new();
        let mut cached_warnings = Vec::new();
        self.assemble_sources_to_rebuild(paths, obj_paths, &mut cached_warnings, pch, &mut jobs)?;
        let pch_option = if pch { PchOption::UsePch } else { PchOption::NoPch };
        let mut job_futures = Vec::new();
        for job in jobs {
            let obj_path = self.get_artifact_path(&job, &self.objs_path, "obj");
            let mut obj_subdir_path = obj_path;
            obj_subdir_path.pop();
            fs::create_dir_all(&obj_subdir_path).unwrap();
            let objs_path = self.objs_path.clone();

            let fut = self.compile(job, objs_path, pch_option);
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
                println!("{}", warning);
            }
        }

        if fail > 0 {
            println!("Compiled: {}/{} | Failed: {}/{}", succ, num_jobs, fail, num_jobs);
        }
        res
    }

    fn discover_src_deps(&mut self, path: impl AsRef<Path>) -> Result<Option<Vec<PathBuf>>, BuildError> {
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
    
    async fn compile(&self, path: impl AsRef<Path>, obj_path: impl AsRef<Path>, pch: PchOption) -> Result<(), BuildError> {
        let path = path.as_ref();
        let host = Platform::host();
        let flags = match host.os() {
            Os::Windows => {
                let mut flags = CompileFlags::empty()
                    .singles([
                        "/W3",
                        "/Zi",
                        "/EHsc",
                        "/c",
                        "/FS",
                    ])
                    .rtti(self.config.cxx_options.rtti)
                    .async_await(self.config.cxx_options.async_await)
                    .cxx_standard(self.config.cxx_options.standard);

                match self.build_options.compile_mode {
                    CompileMode::Debug => flags = flags.singles(["/MDd", "/RTC1"]),
                    CompileMode::Release => flags = flags.single("/O2"),
                }
                flags = flags
                    .defines(self.definitions.iter().cloned())
                    .include_paths(&self.toolchain_paths.include_paths)
                    .include_paths([
                        &self.dependency_headers_path,
                        &self.src_dir_path,
                    ]);
                match pch {
                    PchOption::GeneratePch | PchOption::UsePch => {
                        let path = self.get_artifact_path(self.src_dir_path.join("pch.h"), &obj_path, "pch");
                        flags = flags.pch_path(path, matches!(pch, PchOption::GeneratePch));
                    },
                    _ => {}
                }
                let src_deps_json_path = self.get_artifact_path(&path, &self.src_deps_path, "json");
                let src_deps_parent = src_deps_json_path.parent().unwrap();
                fs::create_dir_all(src_deps_parent)?;
                flags = flags
                    .obj_path(self.get_artifact_path(path, &obj_path, "obj"))
                    .double("/Fd", self.objs_path.join(&format!("{}.pdb", &self.config.name)))
                    .double("/sourceDependencies", src_deps_json_path)
                    .src_path(path);
                flags
            },
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<CompilerOutput>();
        let unique_output = self.unique_compiler_output.clone();
        let handle = task::spawn(async move {
            // let unique_output = ;
            let mut warning_cache = WarningCache::default();
            while let Some(output) = rx.recv().await {
                match &output {
                    CompilerOutput::Begun { first_line } => println!("{}", first_line),
                    CompilerOutput::Error(s) | CompilerOutput::Warning(s) => {
                        if unique_output.lock().unwrap().insert(s.lines().next().unwrap().to_string()) {
                            println!("{}", s);
                        }
                        if matches!(output, CompilerOutput::Warning(_)) {
                            warning_cache.warnings.push(s.clone());
                        }
                    }
                }
            }
            warning_cache
        });

        let val = if compile_cxx(&self.toolchain_paths, flags, tx).await {
            Ok(())
        } else {
            Err(BuildError::CompilerError)
        };
        let warning_cache = handle.await.unwrap();
        let warning_cache_path = self.get_artifact_path(path, &self.warning_cache_path, "warnings");
        if let Some(parent) = warning_cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let warning_cache = serde_json::to_string(&warning_cache).unwrap();
        fs::write(warning_cache_path, warning_cache)?;
        val
    }

    pub fn link(
        &mut self,
        output_path: impl AsRef<Path>,
        obj_paths: impl IntoIterator<Item=impl AsRef<Path>> + Clone,
    ) -> Result<bool, BuildError> {
        println!("Linking {}...", output_path.as_ref().to_string_lossy());
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
        if matches!(self.config.output_type, OutputType::StaticLibrary) {
            run_cmd("lib.exe", &args, &self.toolchain_paths.bin_paths, BuildError::LinkerError)?;
            Ok(output_path.exists())
        } else {
            for path in &self.config.link_libraries {
                args.push(path.into());
            }
            run_cmd("link.exe", &args, &self.toolchain_paths.bin_paths, BuildError::LinkerError)?;
            Ok(true)
        }
    }
}
