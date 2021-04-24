use std::process::Command;
use std::path::{Path, PathBuf};
use std::convert::AsRef;
use std::fs;
use std::ffi::OsStr;

enum Host {
    Windows,
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
    fn new(host: Host, include_paths: impl IntoIterator<Item=impl AsRef<Path>>, definitions: impl IntoIterator<Item=impl AsRef<str>>, compiler_path: impl Into<PathBuf>, linker_path: impl Into<PathBuf>) -> Self {
        let compiler_flags = match host {
            Host::Windows => {
                let mut flags = vec![
                    "/W3".to_string(),
                    "/GR".to_string(),
                    "/EHsc".to_string(),
                    "/std:c++latest".to_string(),
                    "/c".to_string()
                ];
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

    fn compile(&self, path: impl AsRef<Path>) -> Result<String, CompilerError> {
        let mut args = self.compiler_flags.clone();
        args.push(path.as_ref().to_str().unwrap().to_string());
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

fn main() {
    if !cfg!(target_os = "windows") {
        println!("Unsupported host OS: only Windows is supported.");
        std::process::exit(1);
    }

    let compiler_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\bin\Hostx86\x86\cl.exe";
    let linker_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Tools\MSVC\14.24.28314\bin\Hostx86\x86\link.exe";
    let env = Environment::new(
        Host::Windows,
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
    let src_paths: Vec<_> = fs::read_dir("src").unwrap().filter_map(|entry| {
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
        match env.compile(path) {
            Ok(message) => print!("Compiling {}", message),
            Err(error) => {
                println!(
                    "Compiling {}Compilation failed{}.{}",
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
