use std::process::Command;
use std::path::{Path, PathBuf};
use std::convert::AsRef;
use std::fs::{self, File};
use std::ffi::OsStr;
use std::io::{self, ErrorKind as IoErrorKind};
use std::os::windows::prelude::*;
use std::str::FromStr;
use std::io::{BufReader, Write};
use std::borrow::Cow;
use std::cmp::Ordering;

use serde::{Serialize, Deserialize};
use clap::Clap;

enum Host {
    Windows,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum CxxStandard {
    #[serde(rename="c++11")]
    Cxx11,
    #[serde(rename="c++14")]
    Cxx14,
    #[serde(rename="c++17")]
    Cxx17,
    #[serde(rename="c++20")]
    Cxx20,
}

impl Default for CxxStandard {
    fn default() -> Self {
        CxxStandard::Cxx20
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
enum OutputType {
    GuiApp,
    ConsoleApp,
}

#[derive(Serialize, Deserialize)]
struct ProjectConfig {
    name: String,
    cxx_options: CxxOptions,
    output_type: OutputType,
    link_libraries: Vec<String>,
}

#[derive(Clap, Clone, Copy)]
enum CompileMode {
    Debug,
    Release,
}

impl FromStr for CompileMode {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debug" => Ok(CompileMode::Debug),
            "release" => Ok(CompileMode::Release),
            _ => Err("no match"),
        }
    }
}

struct Environment {
    compiler_flags: Vec<String>,
    linker_flags: Vec<String>,
    compiler_path: PathBuf,
    linker_path: PathBuf,

    src_dir_path: PathBuf,
    objs_path: PathBuf,
}

#[derive(Debug)]
struct BuildError {
    code: Option<i32>,
    message: String,
}

#[derive(Clap)]
struct BuildOptions {
    #[clap(default_value="debug")]
    compile_mode: CompileMode,
}

#[derive(Clap)]
enum Subcommand {
    Init {
        project_root: Option<PathBuf>,
    },
    Build(BuildOptions),
    Run(BuildOptions),
    Debug(BuildOptions),
    Clean,
    Kill,
}

#[derive(Clap)]
struct Options {
    #[clap(subcommand)]
    sub_command: Subcommand,
}

impl Environment {
    fn new(
        host: Host,
        compile_mode: CompileMode,
        options: CxxOptions,
        output_type: OutputType,
        include_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        lib_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        definitions: impl IntoIterator<Item=impl AsRef<str>>,
        compiler_path: impl Into<PathBuf>, linker_path: impl Into<PathBuf>,
        src_dir_path: impl Into<PathBuf>,
        objs_path: impl Into<PathBuf>,
    ) -> Self {
        let compiler_flags = match host {
            Host::Windows => {
                let mut flags = vec![
                    "/W3".to_string(),
                    "/Zi".to_string(),
                    "/EHsc".to_string(),
                    "/c".to_string()
                ];
                if options.rtti {
                    flags.push("/GR".to_string());
                } else {
                    flags.push("/GR-".to_string());
                }
                if options.async_await {
                    flags.push("/await".to_string());
                }
                match options.standard {
                    CxxStandard::Cxx11 | CxxStandard::Cxx14 => flags.push("/std:c++14".to_string()),
                    CxxStandard::Cxx17 => flags.push("/std:c++17".to_string()),
                    CxxStandard::Cxx20 => flags.push("/std:c++latest".to_string()),
                }
                match compile_mode {
                    CompileMode::Debug => {
                        flags.push("/MDd".to_string());
                        flags.push("/RTC1".to_string());
                    },
                    CompileMode::Release => {
                        flags.push("/O2".to_string());
                    },
                }
                for definition in definitions {
                    flags.push(format!("/D{}", definition.as_ref()));
                }
                for path in include_paths {
                    flags.push("/I".to_string());
                    flags.push(path.as_ref().to_str().unwrap().to_string());
                }
                flags
            },
        };
        let linker_flags = match host {
            Host::Windows => {
                let mut flags = vec![
                    "/nologo".to_string(),
                    "/debug".to_string(),
                ];
                flags.push(
                    format!(
                        "/SUBSYSTEM:{}",
                        match output_type {
                            OutputType::GuiApp => "WINDOWS",
                            OutputType::ConsoleApp => "CONSOLE",
                        },
                    )
                );
                for path in lib_paths {
                    flags.push(format!("/LIBPATH:{}", path.as_ref().to_str().unwrap()));
                }
                flags
            }
        };
        Environment {
            compiler_flags,
            linker_flags,
            compiler_path: compiler_path.into(),
            linker_path: linker_path.into(),
            src_dir_path: src_dir_path.into(),
            objs_path: objs_path.into(),
        }
    }

    fn compile(&self, path: impl AsRef<Path>, obj_path: impl AsRef<Path>) -> Result<String, BuildError> {
        let mut args = self.compiler_flags.clone();
        let path = path.as_ref();
        let relative_path = path.strip_prefix(&self.src_dir_path).unwrap();
        args.push(format!(r"/Fo{}", get_artifact_path(relative_path, &obj_path, "obj").to_str().unwrap()));
        args.push(format!(r"/Fd{}", get_artifact_path(relative_path, &obj_path, "pdb").to_str().unwrap()));
        args.push(path.to_str().unwrap().to_string());
        let output = Command::new(&self.compiler_path)
            .args(&args)
            .output()
            .expect("failed to execute process");
        let stdout = std::str::from_utf8(&output.stdout).unwrap().to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            Err(BuildError {
                code: output.status.code(),
                message: stdout
            })
        }
    }

    fn link(
        &self,
        project_name: &str,
        output_path: impl AsRef<Path>,
        obj_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        lib_paths: impl IntoIterator<Item=impl AsRef<Path>>,
    ) -> Result<String, BuildError> {
        let mut args = self.linker_flags.clone();
        let mut output_path = output_path.as_ref().to_owned();
        output_path.push(project_name);
        output_path.set_extension("exe");
        args.push(format!("/out:{}", output_path.to_str().unwrap().to_string()));
        for path in obj_paths {
            args.push(path.as_ref().to_str().unwrap().to_string());
        }
        for path in lib_paths {
            args.push(path.as_ref().to_str().unwrap().to_string());
        }
        let output = Command::new(&self.linker_path)
            .args(&args)
            .output()
            .expect("failed to execute process");
        let stdout = std::str::from_utf8(&output.stdout).unwrap().to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            Err(BuildError {
                code: output.status.code(),
                message: stdout
            })
        }
    }

    fn compile_directory(&self, paths: &Paths, newest_header: u64, obj_paths: &mut Vec<PathBuf>) -> bool {
        let mut success = true;
        macro_rules! fail {
            ($($t:tt)*) => {
                println!($($t)*);
                success = false;
            }
        }
        fs::create_dir_all(&paths.root).unwrap();
        for path in paths.src_paths.iter() {
            let obj_path = get_artifact_path(path.strip_prefix(&self.src_dir_path).unwrap(), &self.objs_path, "obj");
            obj_paths.push(obj_path.clone());
            let mut needs_compile = true;
            let src_modified = fs::metadata(path).unwrap().last_write_time();
            if let Ok(metadata) = fs::metadata(&obj_path) {
                let obj_modified = metadata.last_write_time();
                if obj_modified > newest_header && obj_modified > src_modified {
                    needs_compile = false;
                }
            }
            if needs_compile {
                let mut obj_subdir_path = obj_path;
                obj_subdir_path.pop();
                fs::create_dir_all(&obj_subdir_path).unwrap();
                match self.compile(path, &self.objs_path) {
                    Ok(message) => print!("Compiled {}", message),
                    Err(error) => {
                        fail!(
                            "Attempted to compile {}Compilation failed{}.",
                            error.message,
                            if let Some(code) = error.code {
                                format!(" with code {}", code)
                            } else {
                                String::new()
                            },
                        );
                    }
                }
            }
        }

        for child in &paths.children {
            success &= self.compile_directory(child, newest_header, obj_paths);
        }

        success
    }
}

#[derive(Default)]
struct Paths {
    root: PathBuf,
    src_paths: Vec<PathBuf>,
    children: Vec<Paths>,
}

fn header_and_src_paths(root: PathBuf, header_paths: &mut Vec<PathBuf>, entries: impl IntoIterator<Item=io::Result<fs::DirEntry>>) -> Paths {
    let mut paths = Paths::default();
    paths.root = root;
    for entry in entries {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        if file_type.is_file() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("cpp")) || path.extension() == Some(OsStr::new("cxx")) || path.extension() == Some(OsStr::new("cc")) {
                paths.src_paths.push(path);
            } else if path.extension() == Some(OsStr::new("h")) || path.extension() == Some(OsStr::new("hpp")) || path.extension() == Some(OsStr::new("hxx")) {
                header_paths.push(path);
            }
        } else if file_type.is_dir() {
            let path = entry.path();
            let entries = fs::read_dir(&path).unwrap();
            let child = header_and_src_paths(path, header_paths, entries);
            paths.children.push(child);
        }
    }
    paths
}

fn get_artifact_path(src_path: impl AsRef<Path>, output_dir_path: impl AsRef<Path>, extension: &'static str) -> PathBuf {
    let mut path = output_dir_path.as_ref().to_owned();
    path.push(src_path);
    let succ = path.set_extension(extension);
    assert!(succ);
    path
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct CxxOptions {
    rtti: bool,
    async_await: bool,
    standard: CxxStandard,
}

impl Default for CxxOptions {
    fn default() -> Self {
        CxxOptions {
            rtti: false,
            async_await: true,
            standard: CxxStandard::Cxx20,
        }
    }
}

fn kill_debugger() {
    let _output = Command::new("taskkill")
        .args(&["/IM", "devenv.exe", "/F"])
        .output();
}

struct ToolchainPaths {
    compiler_path: PathBuf,
    linker_path: PathBuf,
    debugger_path: PathBuf,
    include_paths: Vec<PathBuf>,
    lib_paths: Vec<PathBuf>,
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
            parse_version(entry.unwrap().file_name().to_str().unwrap())
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
    fn find() -> io::Result<ToolchainPaths> {
        let mut path = PathBuf::from(r"C:\Program Files (x86)");
        let program_files = path.clone();
        path.push("Microsoft Visual Studio");
        let year = fs::read_dir(&path)?.filter_map(|entry| {
            if let Ok(entry) = entry {
                if let Ok(file_type) = entry.file_type() {
                    let name = entry.path().file_name().unwrap().to_str().unwrap().to_string();
                    if let Ok(number) = name.parse::<u16>() {
                        if file_type.is_dir() {
                            Some((name, number))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
            .max_by_key(|&(ref _name, year)| year)
            .map(|(name, _year)| name)
            .unwrap();
        path.push(year);
        path.push("Community");
        let community = path.clone();

        path.push("VC");
        path.push("Tools");
        path.push("MSVC");

        // TODO: error handling
        path.push(newest_version::<_, 3>(&path).unwrap());
        let version = path.clone();

        path.push("bin");
        path.push("Hostx64");
        path.push("x64");
        let bin = path.clone();

        path.push("cl.exe");
        let compiler_path = path;

        let mut path = bin;
        path.push("link.exe");
        let linker_path = path;

        let mut include_paths = Vec::new();
        let mut lib_paths = Vec::new();
        let mut path = version.clone();
        path.push("ATLMFC");

        let atlmfc = path.clone();
        path.push("include");
        include_paths.push(path);

        let mut path = atlmfc;
        path.push("lib");
        path.push("x64");
        lib_paths.push(path);

        let mut path = version.clone();
        path.push("include");
        include_paths.push(path);

        let mut path = version;
        path.push("lib");
        path.push("x64");
        lib_paths.push(path);

        let mut path = community;
        path.push("Community");
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
        include_paths.push(path.clone());
        for name in &["ucrt", "shared", "um", "winrt", "cppwinrt"] {
            path.push(name);
            include_paths.push(path.clone());
            path.pop();
        }

        let mut path = win10.clone();
        path.push("Lib");
        // TODO: error handling
        path.push(newest_version::<_, 4>(&path).unwrap());
        for name in &["ucrt", "um"] {
            path.push(name);
            path.push("x64");
            lib_paths.push(path.clone());
            path.pop();
            path.pop();
        }

        Ok(
            ToolchainPaths {
                compiler_path,
                linker_path,
                debugger_path,
                include_paths,
                lib_paths,
            }
        )
    }
}

fn main() {
    if !cfg!(target_os = "windows") {
        panic!("Unsupported host OS: only Windows is supported.");
    }

    let options = Options::parse();
    let mut success = true;
    macro_rules! _task_failed {
        () => {
            println!(
                "\n{} failed.",
                match options.sub_command {
                    Subcommand::Init { .. } => "Initialization",
                    Subcommand::Build(_) | Subcommand::Run(_) | Subcommand::Debug(_) => "Build",
                    Subcommand::Clean => "Clean",
                    Subcommand::Kill => "Kill",
                },
            );
            std::process::exit(1);
        }
    }
    macro_rules! check_success {
        () => {
            if !success {
                _task_failed!();
            }
        }
    }
    macro_rules! fail_immediate {
        ($($t:tt)*) => {{
            println!($($t)*);
            _task_failed!();
        }}
    }
    let (config, toolchain_paths, mut artifact_path) = match &options.sub_command {
        Subcommand::Init { project_root } => {
            let project_root: Cow<Path> = project_root.as_ref()
                .map(|path| Cow::from(path.as_path()))
                .unwrap_or_else(||
                    Cow::from(std::env::current_dir().unwrap())
                );
            fs::create_dir_all(&project_root)
                .unwrap_or_else(|error| fail_immediate!("Unable to create project directory: {}.", error));
            let config_path = project_root.join("abs.json");
            if config_path.is_file() {
                fail_immediate!("ABS project already exists.");
            } else {
                let config = ProjectConfig {
                    name: project_root.file_name().unwrap().to_str().unwrap().to_string(),
                    cxx_options: CxxOptions::default(),
                    output_type: OutputType::ConsoleApp,
                    link_libraries: vec![],
                };
                let project_file = File::create(&config_path)
                    .unwrap_or_else(|error| fail_immediate!("Unable to open project file for writing: {}.", error));
                serde_json::to_writer_pretty(project_file, &config).unwrap();

                let mut src_path = project_root.join("src");
                fs::create_dir_all(&src_path).unwrap();
                src_path.push("main.cpp");
                let mut file = fs::File::create(&src_path).unwrap();
                write!(
                    file,
r##"#include <stdio.h>

int main() {{
    printf("Hello, world!\n");
}}
"##
                ).unwrap();
                return;
            }
        },
        Subcommand::Build(build_options) | Subcommand::Run(build_options) | Subcommand::Debug(build_options) => {
            let config_file = match File::open("abs.json") {
                Ok(file) => BufReader::new(file),
                Err(error) => fail_immediate!("Unable to read project file in current working directory: {}.", error),
            };
            let config: ProjectConfig = serde_json::from_reader(config_file)
                .unwrap_or_else(|error| fail_immediate!("Failed to parse project file: {}", error));

            let src_dir_path = Path::new("src");
            let src_dir = match fs::read_dir(src_dir_path) {
                Ok(src_dir) => src_dir,
                Err(error) => {
                    if let IoErrorKind::NotFound = error.kind() {
                        fail_immediate!("src directory does not exist in current working directory.");
                    } else {
                        fail_immediate!("Unable to read src directory in current working directory.");
                    }
                }
            };

            print!("Finding toolchain paths...");
            let toolchain_paths = ToolchainPaths::find().unwrap();
            println!("complete.");
            
            // Create abs/debug or abs/release, if it doesn't exist already
            let artifact_subdirectory = match build_options.compile_mode {
                CompileMode::Debug => "debug",
                CompileMode::Release => "release",
            };
            let artifact_path: PathBuf = ["abs", artifact_subdirectory].iter().collect();
            let mut objs_path = artifact_path.clone();
            objs_path.push("obj");
            if let Some(kind) = fs::create_dir_all(&objs_path).err().map(|error| error.kind()) {
                fail_immediate!(
                    "Unable to create abs directory structure: {:?}.",
                    match kind {
                        IoErrorKind::PermissionDenied => "permission denied".to_string(),
                        kind => format!("{:?}", kind),
                    }
                );
            }
            
            let mut header_paths = Vec::new();
            let paths = header_and_src_paths(src_dir_path.to_path_buf(), &mut header_paths, src_dir);
            
            let newest_header = header_paths.iter().map(|header| {
                fs::metadata(header).unwrap().last_write_time()
            }).max().unwrap_or(0u64);
            let mut obj_paths = Vec::new();
            let env = Environment::new(
                Host::Windows,
                build_options.compile_mode,
                config.cxx_options,
                config.output_type,
                &toolchain_paths.include_paths,
                &toolchain_paths.lib_paths,
                &["_WINDOWS", "WIN32", "UNICODE", "_USE_MATH_DEFINES"],
                &toolchain_paths.compiler_path,
                &toolchain_paths.linker_path,
                src_dir_path,
                objs_path,
            );
        
            success &= env.compile_directory(&paths, newest_header, &mut obj_paths);
        
            check_success!();
            if let Some(error) = env.link(&config.name, &artifact_path, obj_paths, &config.link_libraries).err() {
                fail_immediate!("{}", error.message);
            }
            println!("Build succeeded.");
            (config, toolchain_paths, artifact_path)
        },
        Subcommand::Clean => {
            fn _cleaned_successfully() { println!("Cleaned successfully."); }
            match fs::remove_dir_all("abs") {
                Ok(()) => _cleaned_successfully(),
                Err(error) => match error.kind() {
                    IoErrorKind::NotFound => _cleaned_successfully(),
                    error => println!("Failed to clean: {:?}.", error),
                }
            }
            return;
        },
        Subcommand::Kill => {
            kill_debugger();
            println!("Successfully killed debugger.");
            return;
        },
    };

    match options.sub_command {
        Subcommand::Run(_) => {
            artifact_path.push(&config.name);
            artifact_path.set_extension("exe");
            Command::new(artifact_path)
                .spawn()
                .unwrap()
                .wait()
                .unwrap();
        },
        Subcommand::Debug(_) => {
            kill_debugger();
            artifact_path.push(&config.name);
            artifact_path.set_extension("exe");
            Command::new(&toolchain_paths.debugger_path)
                .args(&["/debugexe".to_string(), artifact_path.to_str().unwrap().to_string()])
                .spawn()
                .unwrap();
        },
        _ => {},
    }
}
