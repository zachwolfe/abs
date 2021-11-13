use std::process::Command;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::ErrorKind as IoErrorKind;
use std::io::{BufReader, Write};
use std::borrow::Cow;
use std::ffi::OsStr;
use std::collections::HashSet;

use clap::Parser;

mod build;
mod cmd_options;
mod proj_config;

use proj_config::{ProjectConfig, OutputType, CxxOptions, Platform};
use cmd_options::{CmdOptions, CompileMode, Subcommand, BuildOptions};
use build::{BuildEnvironment, ToolchainPaths};

pub fn kill_process(path: impl AsRef<Path>) -> Option<i32> {
    Command::new("taskkill")
        .args(&[OsStr::new("/F"), OsStr::new("/IM"), path.as_ref().as_os_str()])
        .output()
        .map(|output| output.status.code())
        .unwrap_or(None)
}

fn kill_debugger() -> Option<i32> {
    kill_process("devenv.exe")
}

#[cfg(target_os = "windows")]
fn main() {
    use crate::cmd_options::Target;

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
    let (config, artifact_path, toolchain_paths) = match &options.sub_command {
        Subcommand::Init { project_root, output_type } => {
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
                let link_libraries = match output_type {
                    OutputType::ConsoleApp | OutputType::DynamicLibrary => vec![],
                    OutputType::GuiApp => vec!["user32.lib".to_string(), "comctl32.lib".to_string()],
                };
                let config = ProjectConfig {
                    name: project_root.file_name().unwrap()
                        .to_str().expect("Project name must be representable in UTF-8")
                        .to_string(),
                    cxx_options: CxxOptions::default(),
                    output_type: *output_type,
                    link_libraries,
                    supported_targets: vec![Platform::Win32, Platform::Win64],
                };
                let project_file = File::create(&config_path)
                    .unwrap_or_else(|error| fail_immediate!("Unable to open project file for writing: {}.", error));
                serde_json::to_writer_pretty(project_file, &config).unwrap();

                let mut src_path = project_root.join("src");
                fs::create_dir_all(&src_path).unwrap();
                src_path.push("main.cpp");
                let mut file = fs::File::create(&src_path).unwrap();
                match output_type {
                    OutputType::ConsoleApp => {
                        write!(
                            file,
r##"#include <stdio.h>

int main() {{
    printf("Hello, world!\n");
}}
"##
                        ).unwrap();
                    },
                    OutputType::GuiApp => {
                        write!(
                            file,
r##"#include <windows.h>
#include <commctrl.h>

LRESULT CALLBACK WindowProc(HWND hwnd, UINT uMsg, WPARAM wParam, LPARAM lParam);

int WINAPI wWinMain(HINSTANCE hInstance, HINSTANCE hPrevInstance, PWSTR pCmdLine, int nCmdShow) {{
    InitCommonControls();

    const wchar_t CLASS_NAME[] = L"{0} Window Class";
    WNDCLASS wc = {{
        .lpfnWndProc = WindowProc,
        .hInstance = hInstance,
        .hCursor = LoadCursor(nullptr, IDC_ARROW),
        .lpszClassName = CLASS_NAME,
    }};
    RegisterClass(&wc);

    HWND hwnd = CreateWindowEx(
        0,
        CLASS_NAME,
        L"{0}",
        WS_OVERLAPPEDWINDOW,

        CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT,

        NULL,
        NULL,
        hInstance,
        NULL
    );
    if(hwnd == NULL) return 0;

    ShowWindow(hwnd, nCmdShow);

    MSG msg = {{}};
    while(true) {{
        while(PeekMessage(&msg, hwnd, 0, 0, PM_REMOVE)) {{
            TranslateMessage(&msg);
            DispatchMessage(&msg);
        }}
    }}
}}

LRESULT CALLBACK WindowProc(HWND hwnd, UINT uMsg, WPARAM wParam, LPARAM lParam) {{
    switch(uMsg) {{
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    case WM_PAINT:
        {{
            PAINTSTRUCT ps;
            HDC hdc = BeginPaint(hwnd, &ps);

            FillRect(hdc, &ps.rcPaint, (HBRUSH) (COLOR_WINDOW+1));
            EndPaint(hwnd, &ps);
            return 0;
        }}
    }}
    return DefWindowProc(hwnd, uMsg, wParam, lParam);
}}
"##,
                            config.name,
                        ).unwrap()
                    },
                    OutputType::DynamicLibrary => {
                        write!(
                            file,
r##"#include <windows.h>

__declspec(dllexport) BOOL WINAPI DllMain(HINSTANCE hinstDLL, DWORD fdwReason, LPVOID lpReserved) {{
    return TRUE;
}}
"##,
                        ).unwrap();
                    }
                }
                return;
            }
        },
        Subcommand::Build(build_options) | Subcommand::Run(build_options) | Subcommand::Debug(build_options) => {
            let config_path = PathBuf::from("abs.json");
            let config_file = match File::open(&config_path) {
                Ok(file) => BufReader::new(file),
                Err(error) => fail_immediate!("Unable to read project file in current working directory: {}.", error),
            };
            let config: ProjectConfig = serde_json::from_reader(config_file)
                .unwrap_or_else(|error| fail_immediate!("Failed to parse project file: {}", error));

            if matches!(config.output_type, OutputType::DynamicLibrary) && matches!(options.sub_command, Subcommand::Run(_) | Subcommand::Debug(_)) {
                let sub_command_name = match options.sub_command {
                    Subcommand::Run(_) => "run",
                    Subcommand::Debug(_) => "debug",
                    _ => unreachable!(),
                };
                fail_immediate!("`{}` subcommand not supported for dynamic library projects. Consider using the `build` subcommand and linking the result in another executable.", sub_command_name);
            }

            // Validate supported targets list
            if config.supported_targets.is_empty() {
                fail_immediate!("abs.json contains an empty list of supported targets. Please add at least one and try again.\nAvailable options: win32, win64.");
            }
            // TODO: speed
            let unique_supported_targets: HashSet<_> = config.supported_targets.iter().cloned().collect();
            if unique_supported_targets.len() < config.supported_targets.len() {
                fail_immediate!("abs.json contains one or more duplicates in its list of supported targets. Please ensure that each target is unique.\nThe supported platforms listed are: {:?}", config.supported_targets);
            }

            fn build(target: Platform, build_options: &BuildOptions, config: &ProjectConfig, config_path: &Path) -> (PathBuf, ToolchainPaths) {
                let mode = match build_options.compile_mode {
                    CompileMode::Debug => "debug",
                    CompileMode::Release => "release",
                };
                println!("Building {} for target {:?} in {} mode", config.name, target, mode);

                let toolchain_paths = ToolchainPaths::find(target).unwrap();            
                // Create abs/debug or abs/release, if it doesn't exist already
                let mut artifact_path: PathBuf = ["abs", mode].iter().collect();
                artifact_path.push(format!("{:?}", target));
    
                let mut env = BuildEnvironment::new(
                    config,
                    config_path,
                    build_options,
                    &toolchain_paths,
                    // TODO: make these configurable
                    &[["_WINDOWS", ""], ["WIN32", ""], ["UNICODE", ""], ["_USE_MATH_DEFINES", ""]],
                    &artifact_path,
                ).unwrap();
    
                if let Some(error) = env.build().err() {
                    env.fail(error);
                }

                println!("Build succeeded.");
                (artifact_path, toolchain_paths)
            }

            let host = Platform::host();
            let specified_target: Target = build_options.target.into();
            match specified_target {
                Target::All => {
                    if matches!(options.sub_command, Subcommand::Run(_) | Subcommand::Debug(_)) {
                        let sub_command_name = match options.sub_command {
                            Subcommand::Run(_) => "run",
                            Subcommand::Debug(_) => "debug",
                            _ => unreachable!(),
                        };
                        fail_immediate!("Target `all` is not valid for `{}` subcommand. Please use the `build` subcommand instead.", sub_command_name);
                    } else {
                        for &supported_target in &config.supported_targets {
                            build(supported_target, build_options, &config, &config_path);
                        }
                        return;
                    }
                },
                Target::Host => {
                    let mut target = host;
                    let mut can_run_on_host = true;
                    // If the host isn't a supported target, then pick target with which the host is
                    // backwards compatible.
                    if !config.supported_targets.contains(&target) {
                        can_run_on_host = false;
                        let compatible = config.supported_targets.iter().cloned()
                            .find(|&supported_target| host.is_backwards_compatible_with(supported_target));
                        if let Some(compatible) = compatible {
                            target = compatible;
                            can_run_on_host = true;
                        }
                    }

                    if !can_run_on_host {
                        if matches!(options.sub_command, Subcommand::Run(_) | Subcommand::Debug(_)) {
                            let sub_command_name = match options.sub_command {
                                Subcommand::Run(_) => "run",
                                Subcommand::Debug(_) => "debug",
                                _ => unreachable!(),
                            };
                            fail_immediate!("`{}` subcommand cannot proceed because your host platform, {:?}, is not compatible with any of the supported targets in this project's abs.json.\nThe supported platforms listed are: {:?}", sub_command_name, host, config.supported_targets);
                        } else {
                            // Don't need to run, so if there is only one target supported, choose it regardless
                            // of compatibility.
                            if config.supported_targets.len() == 1 {
                                target = config.supported_targets[0];
                            } else {
                                fail_immediate!("Unable to choose a target platform, because there is more than one supported target in this project's abs.json, and none of them are compatible with your host. Please consider specifying a target on the command line (not yet supported).\nThe supported platforms listed are: {:?}", config.supported_targets);
                            }
                        }
                    }
                    let (artifact_path, toolchain_paths) = build(target, build_options, &config, &config_path);
                    (config, artifact_path, toolchain_paths)
                },
                Target::Platform(target) => {
                    if !config.supported_targets.contains(&target) {
                        fail_immediate!("Cannot build for target {:?} because it is not listed as a supported platform in this project's abs.json.\nThe supported platforms listed are: {:?}", target, config.supported_targets);
                    }

                    if !host.is_backwards_compatible_with(target) && matches!(options.sub_command, Subcommand::Run(_) | Subcommand::Debug(_)) {
                        let sub_command_name = match options.sub_command {
                            Subcommand::Run(_) => "run",
                            Subcommand::Debug(_) => "debug",
                            _ => unreachable!(),
                        };
                        fail_immediate!("`{}` subcommand cannot proceed because your host platform, {:?}, is not compatible with the supplied target {:?}. Please use the `build` subcommand instead.", sub_command_name, host, target);
                    }

                    let (artifact_path, toolchain_paths) = build(target, build_options, &config, &config_path);
                    (config, artifact_path, toolchain_paths)
                }
            }
        },
        Subcommand::Clean => {
            for &mode in ["debug", "release"].iter() {
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

    let mut run_path = artifact_path.join(&config.name);
    run_path.set_extension("exe");
    match options.sub_command {
        Subcommand::Run(_) => {
            let mut child = Command::new(run_path)
                .spawn()
                .unwrap();
            match config.output_type {
                OutputType::ConsoleApp => {
                    // Only wait for the process to complete if this is a console app
                    child.wait().unwrap();
                },
                OutputType::GuiApp | OutputType::DynamicLibrary => {}
            }
        },
        Subcommand::Debug(_) => {
            Command::new(&toolchain_paths.debugger_path)
                .args(&[OsStr::new("/debugexe"), run_path.as_os_str()])
                .spawn()
                .unwrap();
        },
        _ => {},
    }
}
