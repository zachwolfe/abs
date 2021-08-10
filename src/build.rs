use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{self, Write};
use std::cmp::Ordering;
use std::process::Command;
use std::ffi::{OsStr, OsString};
use std::os::windows::prelude::*;
use std::iter;
use std::collections::HashMap;
use std::array::IntoIter;
use std::time::Instant;

use super::proj_config::{Host, ProjectConfig, CxxStandard, OutputType};
use super::cmd_options::{BuildOptions, CompileMode};

const MANIFEST_FILE_NAME: &str = "AppxManifest.xml";
// In milliseconds
const PACKAGE_DIR_LOCK_TIMEOUT: u128 = 500;

pub struct WinUiPaths {
    pub foundation_contract_path: PathBuf,

    /// The paths to WinMDs included in the downloaded Nuget packages, plus the UnionMetadata directory
    pub winmd_paths: Vec<PathBuf>,

    pub cppwinrt_path: PathBuf,
    pub midl_path: PathBuf,
    pub mdmerge_path: PathBuf,
    pub makeappx_path: PathBuf,
    pub signtool_path: PathBuf,
}

pub struct ToolchainPaths {
    pub compiler_path: PathBuf,
    pub linker_path: PathBuf,
    pub debugger_path: PathBuf,
    pub include_paths: Vec<PathBuf>,
    pub lib_paths: Vec<PathBuf>,

    pub win_ui_paths: Option<WinUiPaths>,
}

pub struct BuildEnvironment<'a> {
    compiler_flags: Vec<OsString>,
    linker_flags: Vec<OsString>,
    midl_flags: Vec<OsString>,

    midl_dependencies: Vec<PathBuf>,
    linker_lib_dependencies: Vec<PathBuf>,

    toolchain_paths: &'a ToolchainPaths,
    config: &'a ProjectConfig,
    src_dir_path: PathBuf,
    assets_dir_path: PathBuf,
    objs_path: PathBuf,
    unmerged_winmds_path: PathBuf,
    merged_winmds_path: PathBuf,
    generated_sources_path: PathBuf,
    external_projections_path: PathBuf,
    pub package_dir_path: PathBuf,

    file_edit_times: HashMap<PathBuf, u64>,
}

#[derive(Debug)]
pub enum BuildError {
    NoSrcDirectory,
    CantReadSrcDirectory,
    CompilerError,
    LinkerError,
    CppWinRtError,
    MergedWinMDError,
    IdlError,
    NugetError,
    InstallError,
    FileLockTimeoutReached,

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
    pub idl_paths: Vec<PathBuf>,
    pub children: Vec<SrcPaths>,
}

impl SrcPaths {
    // Returns tuple of src paths and header paths.
    pub fn from_root(root: impl Into<PathBuf>) -> io::Result<(SrcPaths, Vec<PathBuf>)> {
        fn src_paths(root: PathBuf, header_paths: &mut Vec<PathBuf>, entries: impl IntoIterator<Item=io::Result<fs::DirEntry>>) -> io::Result<SrcPaths> {
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
                            "idl"                  => paths.idl_paths.push(path),
                            "h"   | "hpp" | "hxx"  => header_paths.push(path),
                            _ => {},
                        }
                    }
                } else if file_type.is_dir() {
                    let path = entry.path();
                    let entries = fs::read_dir(&path)?;
                    let child = src_paths(path, header_paths, entries)?;
                    paths.children.push(child);
                }
            }
            Ok(paths)
        }
        
        let root = root.into();
        let entries = fs::read_dir(&root)?;
        let mut header_paths = Vec::new();
        let src_paths = src_paths(root, &mut header_paths, entries)?;
        Ok((src_paths, header_paths))
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

    fn files_in(mut self, path: impl AsRef<Path>, extension: impl AsRef<OsStr>) -> io::Result<Self> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let path = entry.path();
                if path.extension() == Some(extension.as_ref()) {
                    self.dependencies.push(path);
                }
            }
        }

        Ok(self)
    }

    fn files_in_recursively(mut self, path: impl AsRef<Path>, extension: impl AsRef<OsStr> + Clone) -> io::Result<Self> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_file() {
                let path = entry.path();
                if path.extension() == Some(extension.as_ref()) {
                    self.dependencies.push(path);
                }
            } else if file_type.is_dir() {
                self = self.files_in_recursively(entry.path(), extension.clone())?;
            }
        }

        Ok(self)
    }

    fn build(self) -> Vec<PathBuf> { self.dependencies }
}

fn run_cmd(cmd: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>, error: BuildError) -> Result<(), BuildError> {
    let code = Command::new(cmd)
        .args(args)
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

fn run_ps_cmd(cmd: impl AsRef<OsStr>, args: impl IntoIterator<Item=impl AsRef<OsStr>>, error: BuildError) -> Result<(), BuildError> {
    run_cmd("powershell", get_ps_args(cmd, args), error)
}

#[derive(Copy, Clone)]
enum PchOption {
    GeneratePch,
    UsePch,
    NoPch,
}

impl<'a> BuildEnvironment<'a> {
    pub fn new<'b>(
        host: Host,
        config: &'a ProjectConfig,
        build_options: &BuildOptions,
        toolchain_paths: &'a ToolchainPaths,
        definitions: impl IntoIterator<Item=&'b [impl AsRef<str> + 'b; 2]>,
        artifact_path: impl Into<PathBuf>,
    ) -> Result<Self, BuildError> {
        let artifact_path = artifact_path.into();
        let compiler_flags = match host {
            Host::Windows => {
                let mut flags: Vec<OsString> = vec![
                    "/W3".into(),
                    "/Zi".into(),
                    "/EHsc".into(),
                    "/c".into(),
                ];
                if config.cxx_options.rtti {
                    flags.push("/GR".into());
                } else {
                    flags.push("/GR-".into());
                }
                if config.cxx_options.async_await {
                    flags.push("/await".into());
                }
                match config.cxx_options.standard {
                    CxxStandard::Cxx11 | CxxStandard::Cxx14 => flags.push("/std:c++14".into()),
                    CxxStandard::Cxx17 => flags.push("/std:c++17".into()),
                    CxxStandard::Cxx20 => flags.push("/std:c++latest".into()),
                }
                match build_options.compile_mode {
                    CompileMode::Debug => {
                        flags.push("/MDd".into());
                        flags.push("/RTC1".into());
                    },
                    CompileMode::Release => {
                        flags.push("/O2".into());
                    },
                }
                for definition in definitions {
                    flags.push(format!("/D{}={}", definition[0].as_ref(), definition[1].as_ref()).into());
                }
                for path in &toolchain_paths.include_paths {
                    flags.push("/I".into());
                    flags.push(path.as_os_str().to_owned());
                }
                flags
            },
        };
        let (linker_flags, linker_lib_dependencies) = match host {
            Host::Windows => {
                let mut flags: Vec<OsString> = vec![
                    "/nologo".into(),
                    "/debug".into(),
                ];
                if config.output_type.is_win_ui() {
                    flags.push("/appcontainer".into());
                }
                flags.push(
                    format!(
                        "/SUBSYSTEM:{}",
                        match config.output_type {
                            OutputType::GuiApp | OutputType::WinUiApp => "WINDOWS",
                            OutputType::ConsoleApp => "CONSOLE",
                        },
                    ).into()
                );
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
                for path in &toolchain_paths.lib_paths {
                    flags.push(cmd_flag("/LIBPATH:", path));
                }
                (flags, dependencies.build())
            }
        };
        let (midl_flags, midl_dependencies) = if config.output_type.is_win_ui() {
            // Assumption: unwrapping is safe here, because midl is only used when in WinUI mode
            let win_ui_paths = toolchain_paths.win_ui_paths.as_ref().unwrap();
            let mut flags = vec![
                "/winrt".into(),
                "/metadata_dir".into(),
                win_ui_paths.foundation_contract_path.as_os_str().to_os_string(),
                "/W1".into(),
                "/nologo".into(),
                "/char".into(),
                "signed".into(),
                "/env".into(),
                "win32".into(),
                "/h".into(),
                "nul".into(),
                "/dlldata".into(),
                "nul".into(),
                "/iid".into(),
                "nul".into(),
                "/proxy".into(),
                "nul".into(),
                "/notlb".into(),
                "/client".into(),
                "none".into(),
                "/server".into(),
                "none".into(),
                "/enum_class".into(),
                "/ns_prefix".into(),
                "/target".into(),
                "NT60".into(),
                "/nomidl".into(),
            ];
            let mut dependencies = DependencyBuilder::default()
                .files_in(&win_ui_paths.foundation_contract_path, "winmd").unwrap();
            for winmd_path in &win_ui_paths.winmd_paths {
                for entry in fs::read_dir(winmd_path).unwrap() {
                    let path = entry.unwrap().path();
                    if path.extension() == Some(OsStr::new("winmd")) {
                        flags.push(OsString::from("/reference"));
                        dependencies = dependencies.file(&path);
                        flags.push(path.as_os_str().to_owned());
                    }
                }
            }
            for include_path in &toolchain_paths.include_paths {
                flags.push(OsString::from("/I"));
                flags.push(include_path.as_os_str().to_owned());
                dependencies = dependencies.files_in(include_path, "idl").unwrap();
            }
            (flags, dependencies.build())
        } else {
            Default::default()
        };
        let objs_path = artifact_path.join("obj");
        let unmerged_winmds_path = artifact_path.join("unmerged_metadata");
        let merged_winmds_path = artifact_path.join("merged_metadata");
        let generated_sources_path = artifact_path.join("generated_sources");
        let external_projections_path = artifact_path.join("external_projections");
        let package_dir_path = artifact_path.join("package");
        fs::create_dir_all(&objs_path)?;
        fs::create_dir_all(&package_dir_path)?;

        if config.output_type.is_win_ui() {
            fs::create_dir_all(&unmerged_winmds_path)?;
            fs::create_dir_all(&merged_winmds_path)?;
            fs::create_dir_all(&generated_sources_path)?;
            fs::create_dir_all(&external_projections_path)?;
        }
        
        Ok(BuildEnvironment {
            compiler_flags,
            linker_flags,
            midl_flags,

            midl_dependencies,
            linker_lib_dependencies,

            toolchain_paths,
            config,
            src_dir_path: "src".into(),
            assets_dir_path: "assets".into(),
            objs_path,
            unmerged_winmds_path,
            merged_winmds_path,
            generated_sources_path,
            external_projections_path,
            package_dir_path,

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
            BuildError::CompilerError => println!("unable to compile."),
            BuildError::LinkerError => println!("unable to link."),
            BuildError::CppWinRtError => println!("there was a cppwinrt error."),
            BuildError::IdlError => println!("there was a midl error."),
            BuildError::MergedWinMDError => println!("unable to merge Windows metadata (winmd) files."),
            BuildError::NugetError => println!("unable to fetch nuget dependencies."),
            BuildError::InstallError => println!("unable to install package."),
            BuildError::FileLockTimeoutReached => println!("file lock timeout reached while attempting to copy to package directory."),

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
        // TODO: shouldn't really be necessary to collect in a Vec here.
        let dependencies: Result<Vec<_>, _> = dependency_paths.into_iter()
            .map(|path| self.edit_time(path, u64::MAX))
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

    pub fn build(&mut self, artifact_path: impl AsRef<Path>) -> Result<(), BuildError> {
        let (paths, header_paths) = match SrcPaths::from_root(&self.src_dir_path) {
            Ok(paths) => paths,
            Err(error) => {
                if let io::ErrorKind::NotFound = error.kind() {
                    return Err(BuildError::NoSrcDirectory);
                } else {
                    return Err(BuildError::CantReadSrcDirectory);
                }
            }
        };
        if self.config.output_type.is_win_ui() {
            self.compile_all_idls(&paths)?;
            self.project_winsdk()?;
        }
        let pch = paths.src_paths.iter().any(|path| path.file_name() == Some(OsStr::new("pch.cpp")));
        if pch {
            let pch_path = self.src_dir_path.join("pch.cpp");
            let mut dependencies = DependencyBuilder::default()
                .file(&pch_path)
                .files(header_paths.clone());

            if self.config.output_type.is_win_ui() {
                dependencies = dependencies
                    .files_in(&self.generated_sources_path, "h")?
                    .files_in_recursively(&self.external_projections_path, "h")?;
            }

            let gen_pch_path = self.get_artifact_path(&pch_path, &self.objs_path, "pch");
            if self.should_build_artifact(dependencies.build(), &gen_pch_path)? {
                println!("Generating pre-compiled header");
                self.compile(pch_path, &self.objs_path, PchOption::GeneratePch)?;
            }
        };
        let mut obj_paths = Vec::new();
        self.compile_directory(&paths, header_paths.iter().map(|path| path.as_ref()), &mut obj_paths, pch)?;

        let exe_name = format!("{}.exe", self.config.name);
        let pdb_name = format!("{}.pdb", self.config.name);
        let exe_path = artifact_path.as_ref().join(&exe_name);
        let pdb_path = artifact_path.as_ref().join(&pdb_name);

        let dependencies: Vec<_> = obj_paths.clone().iter().cloned()
            .chain(self.linker_lib_dependencies.iter().cloned())
            .collect();
        let should_relink = self.should_build_artifact(&dependencies, &exe_path)?;
        if should_relink {
            self.link(&exe_path, obj_paths)?;
        }

        let manifest_src_path = self.src_dir_path.join(MANIFEST_FILE_NAME);
        let manifest_output_path = self.package_dir_path.join(MANIFEST_FILE_NAME);
        super::kill_debugger();
        super::kill_process(&exe_name);
        if should_relink || (self.config.output_type.is_win_ui() && self.should_build_artifact(&[&manifest_src_path], &manifest_output_path)?) {
            let mut package_file_paths = vec![exe_path, pdb_path];
            if self.config.output_type.is_win_ui() {
                package_file_paths.extend([
                    manifest_src_path.clone(),
        
                    // TODO: this is a horrible hack!
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\Images".into(),
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\WinUITest".into(),
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\Microsoft.ApplicationModel.Resources.winmd".into(),
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\Microsoft.Internal.FrameworkUdk.dll".into(),
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\Microsoft.ui.xaml.dll".into(),
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\resources.pri".into(),
                    r"C:\Users\zachr\Work\WinUITest\WinUITest (Package)\bin\x86\Debug\AppX\ucrtbased.dll".into(),
                ]);
            }
            if fs::metadata(&self.assets_dir_path)?.is_dir() {
                package_file_paths.push(self.assets_dir_path.clone());
            }
            self.copy_to_package_dir(package_file_paths)?;
            if self.config.output_type.is_win_ui() {
                self.install_package()?;
            }
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

    fn compile_idl(&mut self, path: impl AsRef<Path>, winmd_path: impl AsRef<Path>) -> Result<(), BuildError> {
        let mut flags = self.midl_flags.clone();
        let winmd_path = winmd_path.as_ref();
        let deps: Vec<_> = self.midl_dependencies.iter()
            .cloned()
            .chain(iter::once(path.as_ref().to_owned()))
            .collect();
        if self.should_build_artifact(deps, winmd_path)? {
            flags.push("/winmd".into());
            flags.push(winmd_path.as_os_str().to_os_string());
            flags.push(path.as_ref().as_os_str().to_owned());
            // Assumption: it's safe to unwrap here, because idl files are only supported in WinUI mode
            let code = Command::new(&self.toolchain_paths.win_ui_paths.as_ref().unwrap().midl_path)
                .args(flags)
                .env("PATH", self.toolchain_paths.compiler_path.parent().unwrap())
                .spawn()
                .unwrap()
                .wait()
                .unwrap();
    
            if code.success() {
                Ok(())
            } else {
                Err(BuildError::IdlError)
            }
        } else {
            Ok(())
        }
    }

    pub fn compile_idl_directory_recursive(&mut self, paths: &SrcPaths, winmd_paths: &mut Vec<PathBuf>) -> Result<(), BuildError> {
        for idl_path in &paths.idl_paths {
            let winmd_path = self.get_artifact_path(idl_path, &self.unmerged_winmds_path, "winmd");
            self.compile_idl(idl_path, &winmd_path)?;
            winmd_paths.push(winmd_path);
        }

        for child in &paths.children {
            self.compile_idl_directory_recursive(child, winmd_paths)?;
        }

        Ok(())
    }

    pub fn compile_all_idls(&mut self, paths: &SrcPaths) -> Result<(), BuildError> {
        let mut winmd_paths = Vec::new();
        self.compile_idl_directory_recursive(paths, &mut winmd_paths)?;

        let mut dependencies = DependencyBuilder::default()
            .files(&winmd_paths);
        let mut args = vec![
            "/v".into(),
            "/partial".into(),
            "/o".into(), self.merged_winmds_path.as_os_str().to_owned(),
            "/n:1".into(),
        ];

        // Assumption: it's safe to unwrap here, because idls are only supported in WinUI mode
        let win_ui_paths = self.toolchain_paths.win_ui_paths.as_ref().unwrap();
        for reference in &win_ui_paths.winmd_paths {
            args.push("/metadata_dir".into());
            args.push(reference.as_os_str().to_owned());
            dependencies = dependencies.files_in(reference, "winmd")?;
        }
        for input in winmd_paths {
            args.push("/i".into());
            args.push(input.as_os_str().to_owned());
        }

        let merged_path = self.merged_winmds_path.join(&format!("{}.winmd", &self.config.name));
        if self.should_build_artifact(dependencies.build(), &merged_path)? {
            run_cmd(&win_ui_paths.mdmerge_path, args, BuildError::MergedWinMDError)?;
        }

        let references = win_ui_paths.winmd_paths.iter().cloned();
        let mut dependencies = DependencyBuilder::default();
        for path in &win_ui_paths.winmd_paths {
            dependencies = dependencies.files_in(path, "winmd")?;
        }

        let generated_sources_path = self.generated_sources_path.clone();
        if self.should_build_artifacts(dependencies.clone().build(), &generated_sources_path, IntoIter::new(["h", "cpp"]))? {
            let mut args = vec![
                OsString::from("-output"), generated_sources_path.as_os_str().to_owned(),
                OsString::from("-component"),
                OsString::from("-name"), OsString::from(&self.config.name),
                OsString::from("-prefix"),
                OsString::from("-overwrite"),
                OsString::from("-optimize"),
            ];
            for reference in references {
                args.push("-reference".into());
                args.push(reference.as_os_str().to_owned());
            }
            args.extend(IntoIter::new(["-in".into(), merged_path.as_os_str().to_os_string()]));
            
            run_cmd(&win_ui_paths.cppwinrt_path, args, BuildError::CppWinRtError)
        } else {
            Ok(())
        }
    }

    fn project_winmd(&self, path: impl AsRef<Path>, output_path: impl AsRef<Path>) -> Result<(), BuildError> {
        run_cmd(
            // Assumption: it's safe to unwrap here, because WinMDs are only supported in WinUI mode
            &self.toolchain_paths.win_ui_paths.as_ref().unwrap().cppwinrt_path,
            &[
                OsStr::new("-input"), path.as_ref().as_os_str(),
                OsStr::new("-output"), output_path.as_ref().as_os_str(),
            ],
            BuildError::CppWinRtError,
        )
    }

    fn project_winmd_with_references(&self, path: impl AsRef<Path>, output_path: impl AsRef<Path>, references: impl IntoIterator<Item=impl AsRef<OsStr>>) -> Result<(), BuildError> {
        let mut args = vec![
            OsString::from("-input"), path.as_ref().as_os_str().to_owned(),
            OsString::from("-output"), output_path.as_ref().as_os_str().to_owned(),
        ];
        for reference in references {
            args.push("-reference".into());
            args.push(reference.as_ref().to_owned());
        }
        // Assumption: it's safe to unwrap here, because WinMDs are only supported in WinUI mode
        run_cmd(&self.toolchain_paths.win_ui_paths.as_ref().unwrap().cppwinrt_path, args, BuildError::CppWinRtError)
    }

    fn project_winsdk(&mut self) -> Result<(), BuildError> {
        let mut dependencies = DependencyBuilder::default()
            .files_in_recursively(r"C:\Windows\System32\WinMetadata", "winmd")?;

        // Assumption: it's safe to unwrap here, because WinMDs are only supported in WinUI mode
        let win_ui_paths = self.toolchain_paths.win_ui_paths.as_ref().unwrap();
        for winmd_path in &win_ui_paths.winmd_paths {
            dependencies = dependencies.files_in_recursively(winmd_path, "winmd")?;
        }
        let external_projections_path = self.external_projections_path.clone();
        if self.should_build_artifacts(dependencies.build(), external_projections_path.join("winrt"), IntoIter::new(["h"]))? {
            println!("Projecting the Windows SDK");
            self.project_winmd("sdk", &self.external_projections_path)?;
            for winmd_path in &win_ui_paths.winmd_paths {
                self.project_winmd_with_references(winmd_path, &self.external_projections_path, &["local"])?;
            }
        }

        Ok(())
    }

    pub fn compile_directory<'b>(
        &mut self,
        paths: &'b SrcPaths,
        header_paths: impl IntoIterator<Item=&'b Path> + Clone,
        obj_paths: &mut Vec<PathBuf>,
        pch: bool,
    ) -> Result<(), BuildError> {
        fs::create_dir_all(&paths.root).unwrap();
        let pch_option = if pch { PchOption::UsePch } else { PchOption::NoPch };
        let mut dependencies = DependencyBuilder::default()
            .files(header_paths.clone());
        if self.config.output_type.is_win_ui() {
            dependencies = dependencies
                .files_in(&self.generated_sources_path, "h")?
                .files_in_recursively(&self.external_projections_path, "h")?;
        }
        for path in paths.src_paths.iter() {
            let obj_path = self.get_artifact_path(path, &self.objs_path, "obj");
            obj_paths.push(obj_path.clone());
            let is_pch = path.file_name() == Some(OsStr::new("pch.cpp")) && path.parent() == Some(&self.src_dir_path);
            if !is_pch && self.should_build_artifact(dependencies.clone().file(path).build(), &obj_path)? {
                let mut obj_subdir_path = obj_path;
                obj_subdir_path.pop();
                fs::create_dir_all(&obj_subdir_path).unwrap();
                self.compile(path, &self.objs_path, pch_option)?;
            }
        }

        for child in &paths.children {
            self.compile_directory(child, header_paths.clone(), obj_paths, pch)?;
        }

        Ok(())
    }

    fn compile(&self, path: impl AsRef<Path>, obj_path: impl AsRef<Path>, pch: PchOption) -> Result<(), BuildError> {
        let mut args = self.compiler_flags.clone();
        let path = path.as_ref();

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
        args.push("/I".into());
        args.push(self.generated_sources_path.as_os_str().to_owned());
        args.push("/I".into());
        args.push(self.external_projections_path.as_os_str().to_owned());
        args.push(path.as_os_str().to_owned());
        run_cmd(&self.toolchain_paths.compiler_path, &args, BuildError::CompilerError)?;

        Ok(())
    }

    pub fn link(
        &mut self,
        output_path: impl AsRef<Path>,
        obj_paths: impl IntoIterator<Item=impl AsRef<Path>> + Clone,
    ) -> Result<(), BuildError> {
        println!("Linking {}...", output_path.as_ref().to_string_lossy());
        let mut args = self.linker_flags.clone();
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
        run_cmd(&self.toolchain_paths.linker_path, &args, BuildError::LinkerError)
    }

    fn copy_to_package_dir_recursive(&mut self, file_paths: impl IntoIterator<Item=impl AsRef<Path>>, output: impl AsRef<Path>) -> Result<(), BuildError> {
        for path in file_paths {
            let metadata = fs::metadata(path.as_ref())?;
            let file_name = path.as_ref().file_name().clone().unwrap();
            if metadata.is_dir() {
                let children: Result<Vec<_>, _> = fs::read_dir(path.as_ref())?
                    .map(|entry|
                        entry.map(|entry| entry.path())
                    )
                    .collect();
                let write_dir_path = output.as_ref().join(&file_name);
                fs::create_dir_all(&write_dir_path)?;
                self.copy_to_package_dir_recursive(children?, &write_dir_path)?;
            } else if metadata.is_file() {
                fs::copy(path.as_ref(), output.as_ref().join(&file_name))?;
            }
        }

        Ok(())
    }

    fn copy_to_package_dir(&mut self, file_paths: impl IntoIterator<Item=impl AsRef<Path>>) -> Result<(), BuildError> {
        let package_dir_path = self.package_dir_path.clone();
        // If the debugger or exe was just terminated by ABS, some file locks may persist for a little
        // longer, causing deleting the directory to fail. So, until the timeout, retry repeatedly.
        //
        // TODO: Speed, incrementalism
        let begin_time = Instant::now();
        while fs::remove_dir_all(&package_dir_path).is_err() {
            if Instant::now().duration_since(begin_time).as_millis() > PACKAGE_DIR_LOCK_TIMEOUT {
                return Err(BuildError::FileLockTimeoutReached);
            }
        }
        fs::create_dir_all(&package_dir_path)?;
        self.copy_to_package_dir_recursive(file_paths, package_dir_path)
    }

    fn install_package(&mut self) -> Result<(), BuildError> {
        run_ps_cmd("Add-AppxPackage", &["-Register".into(), self.package_dir_path.join(MANIFEST_FILE_NAME)], BuildError::InstallError)
    }
}

fn get_nuget_path() -> &'static Path {
    let path = Path::new(r"abs\vs\nuget.exe");
    if path.is_file() {
        return path;
    }

    // Otherwise download it off of the internet :(
    print!("Downloading nuget off of the internet...");
    io::stdout().flush().unwrap();
    let mut resp = reqwest::blocking::get("https://dist.nuget.org/win-x86-commandline/latest/nuget.exe").unwrap();
    assert!(resp.status().is_success());
    
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut output = File::create(path).unwrap();
    resp.copy_to(&mut output).unwrap();
    println!("complete.");

    path
}

fn find_nuget_package(name: &str, packages_path: impl AsRef<Path>) -> Option<PathBuf> {
    fs::read_dir(packages_path.as_ref()).unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path|
            path.file_name()
                .unwrap()
                .to_str()
                .map(|file_name| file_name.starts_with(name))
                .unwrap_or(false)
        )
}

fn download_nuget_deps(deps: &[&str]) -> Result<Vec<PathBuf>, BuildError> {
    let nuget_path = get_nuget_path();
    let packages_path = nuget_path.parent().unwrap();
    let mut paths = Vec::new();
    for &dep in deps {
        if let Some(existing) = find_nuget_package(dep, packages_path) {
            paths.push(existing.clone());
            continue;
        }
        println!("Installing {}...", dep);
        run_cmd(
            nuget_path,
            &[
                "install".into(),
                "-OutputDirectory".into(),
                nuget_path.parent().unwrap().to_owned(),
                dep.into(),
            ],
            BuildError::NugetError,
        )?;
        paths.push(find_nuget_package(dep, packages_path).unwrap());
    }
    Ok(paths)
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
    pub fn find_win_ui_paths(win10: &Path, include_paths: &mut Vec<PathBuf>) -> Result<WinUiPaths, BuildError> {
        let dependency_paths = download_nuget_deps(&["Microsoft.Windows.CppWinRT", "Microsoft.ProjectReunion", "Microsoft.ProjectReunion.WinUI", "Microsoft.ProjectReunion.Foundation"])?;
        let mut winmd_paths = Vec::new();
        for md_path in &dependency_paths[2..3] {
            include_paths.push(md_path.join("include"));
            winmd_paths.push(md_path.join(r"lib\uap10.0"));
        }

        let cppwinrt_path = dependency_paths[0].join(r"bin\cppwinrt.exe");

        let mut path = win10.to_path_buf();
        path.push("References");
        path.push(newest_version::<_, 4>(&path).unwrap());
        let mut foundation_contract_path = None;
        for entry in fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() { continue; }

            let mut path = entry.path();
            let name = path.file_name().unwrap().to_os_string();
            let is_foundation_contract = name.to_str()
                .filter(|name| name.to_ascii_lowercase() == "windows.foundation.foundationcontract")
                .is_some();
            if is_foundation_contract {
                path.push(newest_version::<_, 4>(&path).unwrap());
                foundation_contract_path = Some(path);
            }
        }
        let foundation_contract_path = foundation_contract_path.unwrap();

        let mut path = win10.to_path_buf();
        path.push("UnionMetadata");
        path.push(newest_version::<_, 4>(&path).unwrap());
        winmd_paths.push(path.clone());

        let mut path = win10.to_path_buf();
        path.push("bin");
        // TODO: error handling
        path.push(newest_version::<_, 4>(&path).unwrap());
        path.push("x86");
        let midl_path = path.join("midl.exe");
        let mdmerge_path = path.join("mdmerge.exe");
        let makeappx_path = path.join("makeappx.exe");
        let signtool_path = path.join("signtool.exe");

        Ok(
            WinUiPaths {
                foundation_contract_path,
                winmd_paths,

                cppwinrt_path,
                midl_path,
                mdmerge_path,
                makeappx_path,
                signtool_path,
            }
        )
    }

    pub fn find(win_ui_mode: bool) -> Result<ToolchainPaths, BuildError> {
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
        // TODO: principled way of choosing edition
        path.push("Preview");
        let edition = path.clone();

        path.push("VC");
        path.push("Tools");
        path.push("MSVC");

        // TODO: error handling
        path.push(newest_version::<_, 3>(&path).unwrap());
        let version = path.clone();

        path.push("bin");
        path.push("Hostx86");
        path.push("x86");
        let bin = path.clone();

        path.push("cl.exe");
        let compiler_path = path;

        let mut path = bin;
        path.push("link.exe");
        let linker_path = path;

        let mut lib_paths = Vec::new();
        let mut path = version.clone();
        path.push("ATLMFC");

        let atlmfc = path.clone();
        path.push("include");
        let mut include_paths = vec![path];

        let mut path = atlmfc;
        path.push("lib");
        path.push("x86");
        lib_paths.push(path);

        let mut path = version.clone();
        path.push("include");
        include_paths.push(path);

        let mut path = version;
        path.push("lib");
        path.push("x86");
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
        for name in &["ucrt", "shared", "um", "winrt"] {
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
            path.push("x86");
            lib_paths.push(path.clone());
            path.pop();
            path.pop();
        }

        let win_ui_paths = if win_ui_mode {
            Some(ToolchainPaths::find_win_ui_paths(&win10, &mut include_paths)?)
        } else {
            None
        };

        Ok(
            ToolchainPaths {
                compiler_path,
                linker_path,
                debugger_path,
                include_paths,
                lib_paths,

                win_ui_paths,
            }
        )
    }
}
