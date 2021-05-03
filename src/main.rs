use std::process::Command;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::ErrorKind as IoErrorKind;
use std::io::{BufReader, Write};
use std::borrow::Cow;
use std::ffi::OsStr;

use clap::Clap;

mod build;
mod cmd_options;
mod proj_config;

use proj_config::{ProjectConfig, OutputType, Host, CxxOptions};
use cmd_options::{CmdOptions, CompileMode, Subcommand};
use build::{BuildEnvironment, ToolchainPaths};


fn kill_debugger() {
    let _output = Command::new("taskkill")
        .args(&["/IM", "devenv.exe", "/F"])
        .output();
}

fn main() {
    if !cfg!(target_os = "windows") {
        panic!("Unsupported host OS: only Windows is supported.");
    }

    let options = CmdOptions::parse();
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
                    name: project_root.file_name().unwrap()
                        .to_str().expect("Project name must be representable in UTF-8")
                        .to_string(),
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

            print!("Finding toolchain paths...");
            let toolchain_paths = ToolchainPaths::find().unwrap();
            println!("done.");
            
            // Create abs/debug or abs/release, if it doesn't exist already
            let artifact_subdirectory = match build_options.compile_mode {
                CompileMode::Debug => "debug",
                CompileMode::Release => "release",
            };
            let artifact_path: PathBuf = ["abs", artifact_subdirectory].iter().collect();
            let objs_path = artifact_path.join("obj");
            if let Some(kind) = fs::create_dir_all(&objs_path).err().map(|error| error.kind()) {
                fail_immediate!(
                    "Unable to create abs directory structure: {:?}.",
                    match kind {
                        IoErrorKind::PermissionDenied => "permission denied".to_string(),
                        kind => format!("{:?}", kind),
                    }
                );
            }

            let local_winmds_path = artifact_path.join("winmd");
            if let Some(kind) = fs::create_dir_all(&local_winmds_path).err().map(|error| error.kind()) {
                fail_immediate!(
                    "Unable to create winmd directory structure: {:?}.",
                    match kind {
                        IoErrorKind::PermissionDenied => "permission denied".to_string(),
                        kind => format!("{:?}", kind),
                    }
                );
            }

            let mut env = BuildEnvironment::new(
                Host::Windows,
                &config,
                &build_options,
                &toolchain_paths,
                &[["_WINDOWS", ""], ["WIN32", ""], ["UNICODE", ""], ["_USE_MATH_DEFINES", ""]],
                objs_path,
                local_winmds_path,
            );

            if let Some(error) = env.build(&artifact_path).err() {
                fail_immediate!("{:?}", error);
            }

            println!("Build succeeded.");
            (config, toolchain_paths, artifact_path)
        },
        Subcommand::Clean => {
            for mode in ["debug", "release"].iter() {
                if let Err(error) = fs::remove_dir_all(Path::new("abs/").join(mode)) {
                    match error.kind() {
                        IoErrorKind::NotFound => {},
                        error => fail_immediate!("Failed to clean: {:?}.", error),
                    }
                }
            }
            println!("Cleaned successfully.");
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
                .args(&[OsStr::new("/debugexe"), artifact_path.as_os_str()])
                .spawn()
                .unwrap();
        },
        _ => {},
    }
}
