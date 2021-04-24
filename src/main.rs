use std::process::Command;
use std::path::{Path, PathBuf};
use std::convert::AsRef;
use std::fs;
use std::ffi::OsStr;
use std::io::ErrorKind as IoErrorKind;

enum Host {
    Windows,
}

#[derive(Clone, Copy)]
enum CompileMode {
    Debug,
    Release,
}

enum CxxStandard {
    Cxx11,
    Cxx14,
    Cxx17,
    Cxx20,
}

struct Environment {
    compiler_flags: Vec<String>,
    compiler_path: PathBuf,
    linker_path: PathBuf,
}

#[derive(Debug)]
struct CompilerError {
    code: Option<i32>,
    message: String,
}


impl Environment {
    fn new(
        host: Host,
        compile_mode: CompileMode,
        options: CxxOptions,
        include_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        definitions: impl IntoIterator<Item=impl AsRef<str>>,
        compiler_path: impl Into<PathBuf>, linker_path: impl Into<PathBuf>
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
        Environment {
            compiler_flags,
            compiler_path: compiler_path.into(),
            linker_path: linker_path.into(),
        }
    }

    fn compile(&self, path: impl AsRef<Path>, obj_path: impl AsRef<Path>) -> Result<String, CompilerError> {
        let mut args = self.compiler_flags.clone();
        let path = path.as_ref();
        let obj_path = obj_path.as_ref().to_str().unwrap();
        args.push(format!(r"/Fo{}\", obj_path));
        args.push(format!(r"/Fd{}\{}.pdb", obj_path, path.file_stem().unwrap().to_str().unwrap().to_string()));
        args.push(path.to_str().unwrap().to_string());
        let output = Command::new(&self.compiler_path)
            .args(&args)
            .output()
            .expect("failed to execute process");
        let stdout = std::str::from_utf8(&output.stdout).unwrap().to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            Err(CompilerError {
                code: output.status.code(),
                message: stdout
            })
        }
    }
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

fn main() {
    if !cfg!(target_os = "windows") {
        println!("Unsupported host OS: only Windows is supported.");
        std::process::exit(1);
    }

    let compiler_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\bin\Hostx86\x86\cl.exe";
    let linker_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\bin\Hostx86\x86\link.exe";
    let compile_mode = CompileMode::Debug;
    let env = Environment::new(
        Host::Windows,
        compile_mode,
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
        &["_WINDOWS", "WIN32", "UNICODE", "_USE_MATH_DEFINES"],
        compiler_path,
        linker_path,
    );
    let src_dir = match fs::read_dir("src") {
        Ok(src_dir) => src_dir,
        Err(error) => {
            if let IoErrorKind::NotFound = error.kind() {
                println!("src directory does not exist in current working directory. Cannot build.");
            } else {
                println!("Unable to read src directory in current working directory. Cannot build.");
            }
            std::process::exit(1);
        }
    };

    // Create abs/debug or abs/release, if it doesn't exist already
    let artifact_subdirectory = match compile_mode {
        CompileMode::Debug => "debug",
        CompileMode::Release => "release",
    };
    let artifact_path: PathBuf = ["abs", artifact_subdirectory].iter().collect();
    let mut obj_path = artifact_path.clone();
    obj_path.push("obj");
    if let Some(kind) = fs::create_dir_all(&obj_path).err().map(|error| error.kind()) {
        println!(
            "Unable to create abs directory structure: {:?}. Cannot build.",
            match kind {
                IoErrorKind::PermissionDenied => "permission denied".to_string(),
                kind => format!("{:?}", kind),
            }
        );
        std::process::exit(1);
    }

    let src_paths: Vec<_> = src_dir.filter_map(|entry| {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("cpp")) {
                return Some(entry.path());
            }
        }

        None
    }).collect();

    let mut success = true;
    for (i, path) in src_paths.iter().enumerate() {
        match env.compile(path, &obj_path) {
            Ok(message) => print!("Compiled {}", message),
            Err(error) => {
                println!(
                    "Attempted to compile {}Compilation failed{}.{}",
                    error.message,
                    if let Some(code) = error.code {
                        format!(" with code {}", code)
                    } else {
                        String::new()
                    },
                    if i == src_paths.len()-1 {
                        ""
                    } else {
                        "\n"
                    }
                );
                success = false;
            }
        }
    }
    if success {
        println!("\nBuild succeeded.");
    } else {
        println!("\nBuild failed.");
        std::process::exit(2);
    }
}
