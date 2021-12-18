use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::cmp::Ordering;
use std::process::Command;
use std::ffi::{OsStr, OsString};
use std::os::windows::prelude::*;
use std::collections::HashMap;
use std::array::IntoIter;
use std::time::SystemTime;
use std::iter::once;

use serde::Deserialize;

use super::proj_config::{Platform, Os, Arch, ProjectConfig, CxxStandard, OutputType};
use super::cmd_options::{BuildOptions, CompileMode};

pub struct ToolchainPaths {
    pub debugger_path: PathBuf,
    pub include_paths: Vec<PathBuf>,
    pub lib_paths: Vec<PathBuf>,
    pub bin_paths: Vec<PathBuf>,
}

pub struct BuildEnvironment<'a> {
    config_path: PathBuf,
    manifest_path: Option<PathBuf>,

    linker_lib_dependencies: Vec<PathBuf>,
    
    toolchain_paths: &'a ToolchainPaths,
    config: &'a ProjectConfig,
    build_options: &'a BuildOptions,
    definitions: &'a [(&'a str, &'a str)],
    artifact_path: PathBuf,
    src_dir_path: PathBuf,
    assets_dir_path: PathBuf,
    objs_path: PathBuf,
    src_deps_path: PathBuf,

    file_edit_times: HashMap<PathBuf, u64>,
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
        fs::create_dir_all(&objs_path)?;
        fs::create_dir_all(&src_deps_path)?;

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
            artifact_path,
            src_dir_path: "src".into(),
            assets_dir_path: "assets".into(),
            objs_path,
            src_deps_path,

            file_edit_times: HashMap::new(),
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

    pub fn build(&mut self) -> Result<(), BuildError> {
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
                self.compile(pch_path, &self.objs_path, PchOption::GeneratePch)?;
            }
        };
        let mut obj_paths = Vec::new();
        self.compile_directory(&paths, &mut obj_paths, pch)?;

        let product_is_executable = matches!(self.config.output_type, OutputType::ConsoleApp | OutputType::GuiApp);
        let product_name = format!("{}.{}", self.config.name, if product_is_executable { "exe" } else { "dll" });
        let pdb_name = format!("{}.pdb", self.config.name);
        let product_path = self.artifact_path.join(&product_name);
        let pdb_path = self.artifact_path.join(&pdb_name);

        let dependencies: Vec<_> = obj_paths.clone().iter().cloned()
            .chain(self.linker_lib_dependencies.iter().cloned())
            .chain(self.manifest_path.iter().cloned())
            .collect();

        if !matches!(self.config.output_type, OutputType::DynamicLibrary) {
            super::kill_debugger();
            super::kill_process(&product_name);
    
            // File locks may continue to be held on the product for some time after it is
            // terminated/unloaded, causing linking to fail. So, while the exit code is 1, keep trying
            // to kill.
            //
            // This is kind of a hack, but it seems to work well enough.
            while super::kill_debugger() == Some(1) {}
            while super::kill_process(&product_name) == Some(1) {}
        }
            
        let should_relink = self.should_build_artifact(&dependencies, &product_path)?;
        if should_relink {
            self.link(&product_path, obj_paths)?;
        }

        let mut package_file_paths = vec![product_path, pdb_path];
        if self.assets_dir_path.exists() && fs::metadata(&self.assets_dir_path)?.is_dir() {
            package_file_paths.push(self.assets_dir_path.clone());
        }
        Ok(())
    }

    /// Goes from a src file path to an artifact path relative to output_dir_path
    /// (e.g., src/hello/world.cpp -> abs/debug/obj/hello/world.obj)
    fn get_artifact_path(&self, src_path: impl AsRef<Path>, output_dir_path: impl AsRef<Path>, extension: impl AsRef<OsStr>) -> PathBuf {
        let mut path = output_dir_path.as_ref().to_owned();
        // Isolate the src file name
        let src_path = src_path.as_ref().strip_prefix(&self.src_dir_path)
            .expect("path to a src file must have src directory as a prefix");
        path.push(src_path);
        let succ = path.set_extension(extension);
        assert!(succ);
        path
    }

    pub fn compile_directory<'b>(
        &mut self,
        paths: &'b SrcPaths,
        obj_paths: &mut Vec<PathBuf>,
        pch: bool,
    ) -> Result<(), BuildError> {
        fs::create_dir_all(&paths.root).unwrap();
        let pch_option = if pch { PchOption::UsePch } else { PchOption::NoPch };
        for path in paths.src_paths.iter() {
            let obj_path = self.get_artifact_path(path, &self.objs_path, "obj");
            obj_paths.push(obj_path.clone());
            let is_pch = path.file_name() == Some(OsStr::new("pch.cpp")) && path.parent() == Some(&self.src_dir_path);
            let should_rebuild = !is_pch && if let Some(dependencies) = self.discover_src_deps(path)? {
                let dependencies = DependencyBuilder::default()
                    .file(path)
                    .files(dependencies)
                    .build();
                self.should_build_artifact(dependencies, &obj_path)?
            } else {
                true
            };

            if should_rebuild {
                let mut obj_subdir_path = obj_path;
                obj_subdir_path.pop();
                fs::create_dir_all(&obj_subdir_path).unwrap();
                self.compile(path, &self.objs_path, pch_option)?;
            }
        }

        for child in &paths.children {
            self.compile_directory(child, obj_paths, pch)?;
        }

        Ok(())
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

    fn compile(&self, path: impl AsRef<Path>, obj_path: impl AsRef<Path>, pch: PchOption) -> Result<(), BuildError> {
        let path = path.as_ref();

        let host = Platform::host();
        let mut args = match host.os() {
            Os::Windows => {
                let mut flags: Vec<OsString> = vec![
                    "/W3".into(),
                    "/Zi".into(),
                    "/EHsc".into(),
                    "/c".into(),
                ];
                if self.config.cxx_options.rtti {
                    flags.push("/GR".into());
                } else {
                    flags.push("/GR-".into());
                }
                if self.config.cxx_options.async_await {
                    flags.push("/await".into());
                }
                match self.config.cxx_options.standard {
                    CxxStandard::Cxx11 | CxxStandard::Cxx14 => flags.push("/std:c++14".into()),
                    CxxStandard::Cxx17 => flags.push("/std:c++17".into()),
                    CxxStandard::Cxx20 => flags.push("/std:c++latest".into()),
                }
                match self.build_options.compile_mode {
                    CompileMode::Debug => {
                        flags.push("/MDd".into());
                        flags.push("/RTC1".into());
                    },
                    CompileMode::Release => {
                        flags.push("/O2".into());
                    },
                }
                for (name, val) in self.definitions {
                    flags.push(format!("/D{}={}", name, val).into());
                }
                for path in &self.toolchain_paths.include_paths {
                    flags.push("/I".into());
                    flags.push(path.as_os_str().to_owned());
                }
                flags
            },
        };

        println!("Compiling {}", path.as_os_str().to_string_lossy());
        match pch {
            PchOption::GeneratePch | PchOption::UsePch => args.push(
                cmd_flag(
                    "/Fp",
                    self.get_artifact_path(self.src_dir_path.join("pch.h"), &obj_path, "pch"),
                )
            ),
            PchOption::NoPch => {},
        }
        match pch {
            PchOption::GeneratePch => args.push("/Ycpch.h".into()),
            PchOption::UsePch => args.push("/Yupch.h".into()),
            PchOption::NoPch => {},
        }
        args.push(
            cmd_flag(
                "/Fo",
                self.get_artifact_path(path, &obj_path, "obj"),
            )
        );
        args.push(
            cmd_flag(
                "/Fd",
                self.objs_path.join(&format!("{}.pdb", &self.config.name))
            )
        );
        let src_deps_json_path = self.get_artifact_path(&path, &self.src_deps_path, "json");
        args.push(
            cmd_flag(
                "/sourceDependencies",
                src_deps_json_path,
            )
        );
        args.push(path.as_os_str().to_owned());
        run_cmd("cl.exe", &args, &self.toolchain_paths.bin_paths, BuildError::CompilerError)?;

        Ok(())
    }

    pub fn link(
        &mut self,
        output_path: impl AsRef<Path>,
        obj_paths: impl IntoIterator<Item=impl AsRef<Path>> + Clone,
    ) -> Result<(), BuildError> {
        println!("Linking {}...", output_path.as_ref().to_string_lossy());
        let host = Platform::host();
        let mut args = match host.os() {
            Os::Windows => {
                let mut flags: Vec<OsString> = vec![
                    "/nologo".into(),
                    "/debug".into(),
                ];
                flags.push(
                    match self.config.output_type {
                        OutputType::GuiApp => "/SUBSYSTEM:WINDOWS",
                        OutputType::ConsoleApp => "/SUBSYSTEM:CONSOLE",
                        OutputType::DynamicLibrary => "/DLL",
                    }.into()
                );
                flags.push("/manifest:embed".into());
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
                        OutputType::ConsoleApp | OutputType::DynamicLibrary => {},
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
                output_path.as_ref(),
            )
        );
        for path in obj_paths {
            args.push(path.as_ref().as_os_str().to_owned());
        }
        for path in &self.config.link_libraries {
            args.push(path.into());
        }
        run_cmd("link.exe", &args, &self.toolchain_paths.bin_paths, BuildError::LinkerError)
    }
}

fn parse_version<const N: usize>(version: &str) -> Option<[u64; N]> {
    let mut output = [0; N];
    let mut i = 0;
    for component in version.split('.') {
        if i >= N {
            return None;
        }
        output[i] = component.parse::<u64>().ok()?;
        i += 1;
    }
    if i < N {
        None
    } else {
        Some(output)
    }
}

fn newest_version<P: AsRef<Path>, const N: usize>(parent: P) -> Option<PathBuf> {
    fs::read_dir(parent.as_ref()).unwrap()
        .filter_map(|entry| {
            entry.unwrap().file_name().to_str()
                .and_then(parse_version)
        }).max_by(|a: &[u64; N], b: &[u64; N]| {
        for (a, b) in a.iter().zip(b.iter()) {
            match a.cmp(b) {
                Ordering::Greater => return Ordering::Greater,
                Ordering::Less => return Ordering::Less,
                Ordering::Equal => continue,
            }
        }
        Ordering::Equal
    }).map(|path| {
        let mut name = String::new();
        for (i, num) in path.iter().enumerate() {
            if i > 0 {
                name.push('.');
            }
            name.extend(num.to_string().chars());
        }
        PathBuf::from(name)
    })
}

impl ToolchainPaths {
    pub fn find(target: Platform) -> Result<ToolchainPaths, BuildError> {
        let mut path = PathBuf::from(r"C:\Program Files (x86)");
        let program_files = path.clone();
        path.push("Microsoft Visual Studio");
        let year = fs::read_dir(&path)?.filter_map(|entry| {
            entry.ok()
                .filter(|entry| 
                    entry.file_type().ok()
                        .map(|file| file.is_dir())
                        .unwrap_or(false)
                )
                .and_then(|entry|
                    entry.path().file_name().unwrap().to_str()
                        .and_then(|file_name| file_name.parse::<u16>().ok())
                )
        })
            .max()
            .unwrap();
        path.push(year.to_string());
        // Pick the name of the newest folder ("Community", "Preview", etc.).
        // TODO: more principled way of choosing edition.
        let mut edition = OsString::from("Community");
        let mut newest_edition_time = SystemTime::UNIX_EPOCH;
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                let created = metadata.created()?;
                if created > newest_edition_time {
                    newest_edition_time = created;
                    edition = entry.file_name();
                }
            }
        }
        path.push(edition);
        let edition = path.clone();

        path.extend(["VC", "Tools", "MSVC"]);

        // TODO: error handling
        path.push(newest_version::<_, 3>(&path).unwrap());
        let version = path.clone();

        let target = match target.architecture() {
            Arch::X86 => "x86",
            Arch::X64 => "x64",
        };
        let host = if cfg!(target_pointer_width = "64") {
            "x64"
        } else if cfg!(target_pointer_width = "32") {
            "x86"
        } else {
            panic!("Unsupported host pointer width; expected either 32 or 64.");
        };

        let mut bin_paths = Vec::new();

        path.push("bin");
        path.push(format!("Host{}", host));
        path.push(target);
        bin_paths.push(path);

        let mut lib_paths = Vec::new();
        let mut path = version.clone();
        path.push("ATLMFC");

        let atlmfc = path.clone();
        path.push("include");
        let mut include_paths = vec![path];

        let mut path = atlmfc;
        path.push("lib");
        path.push(target);
        lib_paths.push(path);

        let mut path = version.clone();
        path.push("include");
        include_paths.push(path);

        let mut path = version;
        path.push("lib");
        path.push(target);
        lib_paths.push(path);

        let mut path = edition;
        path.push("Common7");
        path.push("IDE");
        path.push("devenv.exe");
        let debugger_path = path;

        let mut path = program_files;
        path.push("Windows Kits");
        path.push("10");
        let win10 = path.clone();

        path.push("Include");
        // TODO: error handling
        path.push(newest_version::<_, 4>(&path).unwrap());
        // include_paths.push(path.clone());
        for &name in &["ucrt", "shared", "um", "winrt"] {
            path.push(name);
            include_paths.push(path.clone());
            path.pop();
        }

        let mut path = win10.clone();
        path.push("Lib");
        // TODO: error handling
        path.push(newest_version::<_, 4>(&path).unwrap());
        for &name in &["ucrt", "um"] {
            path.push(name);
            path.push(target);
            lib_paths.push(path.clone());
            path.pop();
            path.pop();
        }

        let mut path = win10.clone();
        path.push("bin");
        // TODO: error handling
        path.push(newest_version::<_, 4>(&path).unwrap());
        path.push(host);
        bin_paths.push(path);

        Ok(
            ToolchainPaths {
                debugger_path,
                include_paths,
                lib_paths,
                bin_paths,
            }
        )
    }
}
