use std::process::Command;
use std::path::{Path, PathBuf};
use std::convert::AsRef;
use std::fs;
use std::ffi::OsStr;
use std::io::{self, ErrorKind as IoErrorKind};
use std::os::windows::prelude::*;
use std::str::FromStr;

use clap::Clap;

enum Host {
    Windows,
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

enum CxxStandard {
    Cxx11,
    Cxx14,
    Cxx17,
    Cxx20,
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
                    "/subsystem:windows".to_string(),
                ];
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
        output_path: impl AsRef<Path>,
        obj_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        lib_paths: impl IntoIterator<Item=impl AsRef<Path>>,
    ) -> Result<String, BuildError> {
        let mut args = self.linker_flags.clone();
        let mut output_path = output_path.as_ref().to_owned();
        output_path.push("main.exe");
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

struct CxxOptions {
    rtti: bool,
    standard: CxxStandard,
}

impl Default for CxxOptions {
    fn default() -> Self {
        CxxOptions {
            rtti: false,
            standard: CxxStandard::Cxx20,
        }
    }
}

fn kill_debugger() {
    let _output = Command::new("taskkill")
        .args(&["/IM", "devenv.exe", "/F"])
        .output();
}

fn main() {
    let mut success = true;
    fn _build_failed() -> ! {
        println!("\nBuild failed.");
        std::process::exit(1);
    }
    macro_rules! check_success {
        () => {
            if !success {
                _build_failed();
            }
        }
    }
    macro_rules! fail_immediate {
        ($($t:tt)*) => {
            println!($($t)*);
            _build_failed();
        }
    }

    if !cfg!(target_os = "windows") {
        fail_immediate!("Unsupported host OS: only Windows is supported.");
    }

    let options = Options::parse();
    let compiler_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\bin\Hostx86\x86\cl.exe";
    let linker_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\bin\Hostx86\x86\link.exe";
    let devenv_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\Common7\IDE\devenv.exe";
    let mut artifact_path = match &options.sub_command {
        Subcommand::Build(build_options) | Subcommand::Run(build_options) | Subcommand::Debug(build_options) => {
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
                CxxOptions::default(),
                &[
                    r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\ATLMFC\include",
                    r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\include",
                    r"C:\Program Files (x86)\Windows Kits\10\include\10.0.18362.0\ucrt",
                    r"C:\Program Files (x86)\Windows Kits\10\include\10.0.18362.0\shared",
                    r"C:\Program Files (x86)\Windows Kits\10\include\10.0.18362.0\um",
                    r"C:\Program Files (x86)\Windows Kits\10\include\10.0.18362.0\winrt",
                    r"C:\Program Files (x86)\Windows Kits\10\include\10.0.18362.0\cppwinrt",
                ],
                &[
                    r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\ATLMFC\lib\x86",
                    r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\lib\x86",
                    r"C:\Program Files (x86)\Windows Kits\10\lib\10.0.18362.0\ucrt\x86",
                    r"C:\Program Files (x86)\Windows Kits\10\lib\10.0.18362.0\um\x86",
                ],
                &["_WINDOWS", "WIN32", "UNICODE", "_USE_MATH_DEFINES"],
                compiler_path,
                linker_path,
                src_dir_path,
                objs_path,
            );
        
            success &= env.compile_directory(&paths, newest_header, &mut obj_paths);
        
            check_success!();
            let lib_paths = &["avrt.lib"];
            if let Some(error) = env.link(&artifact_path, obj_paths, lib_paths).err() {
                fail_immediate!("{}", error.message);
            }
            println!("Build succeeded.");
            artifact_path
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

    let exe_name = "main.exe";
    match options.sub_command {
        Subcommand::Run(_) => {
            artifact_path.push(exe_name);
            Command::new(artifact_path)
                .spawn()
                .unwrap();
        },
        Subcommand::Debug(_) => {
            kill_debugger();
            artifact_path.push(exe_name);
            Command::new(devenv_path)
                .args(&["/debugexe".to_string(), artifact_path.to_str().unwrap().to_string()])
                .spawn()
                .unwrap();
        },
        _ => {},
    }
}
