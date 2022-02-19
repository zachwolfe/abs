use std::process::Command;
use std::path::{Path, PathBuf, Component, Prefix};
use std::fs::{self, File};
use std::io::ErrorKind as IoErrorKind;
use std::io::{BufReader, Write, Result as IoResult};
use std::borrow::Cow;
use std::ffi::OsStr;
use std::collections::HashSet;
use std::collections::HashMap;
use std::io::Cursor;

use clap::Parser;

mod build;
mod cmd_options;
mod proj_config;
mod build_manager;
mod toolchain_paths;
mod task;
mod progress_bar;

use proj_config::{ProjectConfig, OutputType, CxxOptions, Platform};
use cmd_options::{CmdOptions, CompileMode, Subcommand, Target, BuildOptions};
use build::BuildEnvironment;
use toolchain_paths::ToolchainPaths;

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

// Path::canonicalize() adds an unwanted verbatim prefix on windows. This removes it.
pub fn canonicalize(p: impl AsRef<Path>) -> IoResult<PathBuf> {
    let p = p.as_ref().canonicalize()?;
    let mut components = p.components();
    match components.next() {
        Some(Component::Prefix(prefix)) => {
            let mut ret_val = match prefix.kind() {
                Prefix::VerbatimDisk(letter) => PathBuf::from(format!(r"{}:\", letter as char)),
                _ => PathBuf::new(),
            };
            
            ret_val.extend(components);
            Ok(ret_val)
        },
        _ => Ok(p.to_path_buf()),
    }
}

#[cfg(target_os = "windows")]
#[tokio::main]
async fn main() {
    let options = CmdOptions::parse();
    macro_rules! _task_failed {
        () => {
            println!("\nABS process failed.");
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
                    OutputType::ConsoleApp | OutputType::DynamicLibrary | OutputType::StaticLibrary => vec![],
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
                    dependencies: vec![],
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
                    },
                    OutputType::StaticLibrary => {
                        write!(
                            file,
r##"#include <stdio.h>

void print_hello_world() {{
    printf("Hello, world!");
}}
"##,
                        ).unwrap();
                    },
                }
                return;
            }
        },
        Subcommand::Build(build_options) | Subcommand::Run(build_options) | Subcommand::Debug(build_options) => {
            fn load_config(root_path: &Path) -> (PathBuf, ProjectConfig) {
                let config_path = root_path.join("abs.json");
                let config_file = match File::open(&config_path) {
                    Ok(file) => BufReader::new(file),
                    Err(error) => {
                        let mut err_msg = Cursor::new(Vec::new());
                        let root_path_str = root_path.as_os_str().to_string_lossy();
                        write!(err_msg, "Unable to read project file in ").unwrap();
                        if root_path_str == "." {
                            write!(err_msg, "the current directory").unwrap();
                        } else {
                            write!(err_msg, "directory \"{}\"", root_path_str).unwrap();
                        }
                        fail_immediate!("{}: {}.", String::from_utf8_lossy(&err_msg.into_inner()), error);
                    },
                };
                let config: ProjectConfig = serde_json::from_reader(config_file)
                    .unwrap_or_else(|error| fail_immediate!("Failed to parse project file: {}", error));

                // Validate supported targets list
                if config.supported_targets.is_empty() {
                    fail_immediate!("{} contains an empty list of supported targets. Please add at least one and try again.\nAvailable options: win32, win64.", config_path.as_os_str().to_string_lossy());
                }
                // TODO: speed
                let unique_supported_targets: HashSet<_> = config.supported_targets.iter().cloned().collect();
                if unique_supported_targets.len() < config.supported_targets.len() {
                    fail_immediate!("{} contains one or more duplicates in its list of supported targets. Please ensure that each target is unique.\nThe supported platforms listed are: {:?}", config_path.as_os_str().to_string_lossy(), config.supported_targets);
                }

                (config_path, config)
            }
            let (config_path, config) = load_config(Path::new("."));

            if matches!(config.output_type, OutputType::DynamicLibrary | OutputType::StaticLibrary) && matches!(options.sub_command, Subcommand::Run(_) | Subcommand::Debug(_)) {
                let sub_command_name = match options.sub_command {
                    Subcommand::Run(_) => "run",
                    Subcommand::Debug(_) => "debug",
                    _ => unreachable!(),
                };
                fail_immediate!("`{}` subcommand not supported for library projects. Consider using the `build` subcommand and linking the result in another executable.", sub_command_name);
            }

            struct Project {
                config_path: PathBuf,
                config: ProjectConfig,
                ref_count: u32,
                dep_names: Vec<String>,
                visited: bool,
            }

            let mut projects = HashMap::<String, Project>::new();
            let config_path = match canonicalize(config_path) {
                Ok(canon) => canon,
                Err(_) => fail_immediate!("Failed to get canonical path for project config file"),
            };
            projects.insert(config.name.clone(), Project { config_path: config_path.clone(), config: config.clone(), ref_count: 1, dep_names: Vec::new(), visited: false });

            fn accumulate_dependencies(projects: &mut HashMap<String, Project>, config_path: PathBuf, config: &ProjectConfig) {
                let mut root_path = config_path.clone();
                root_path.pop();

                let canonical_deps: Vec<PathBuf> = config.dependencies.iter()
                    .map(|dep| {
                        if dep.components().count() == 0 {
                            fail_immediate!("Empty path found as dependency in project \"{}\"", config.name);
                        }
                        let dep = root_path.join(dep);
                        match canonicalize(&dep) {
                            Ok(canon) => canon,
                            Err(error) => fail_immediate!("Failed to get canonical path for dependency \"{}\": {}", dep.as_os_str().to_string_lossy(), error),
                        }
                    }).collect();
                let unique_deps: HashSet<&PathBuf> = canonical_deps.iter().collect();
                if unique_deps.len() < canonical_deps.len() {
                    fail_immediate!("{} contains one or more duplicates in its dependencies array", config_path.as_os_str().to_string_lossy());
                }
                let mut dep_names = Vec::new();
                for dependency in &canonical_deps {
                    let (dep_config_path, dep_config) = load_config(dependency);
                    let proj = projects
                        .entry(dep_config.name.clone())
                        .or_insert_with(|| {
                            Project {
                                config_path: dep_config_path.clone(),
                                config: dep_config.clone(),
                                ref_count: 0,
                                dep_names: Vec::new(),
                                visited: false,
                            }
                        });
                    proj.ref_count += 1;
                    // TODO: This is a massive hack! Should think of a more principled way of finding loops.
                    if proj.ref_count > 100 {
                        fail_immediate!("Loop found in dependency graph.");
                    }
                    if dep_config_path != proj.config_path {
                        fail_immediate!("Two projects in dependency graph found with the same name, \"{}\"", proj.config.name);
                    }
                    dep_names.push(proj.config.name.clone());

                    accumulate_dependencies(projects, dep_config_path, &dep_config);
                }

                projects.get_mut(&config.name).unwrap().dep_names = dep_names;
            }
            accumulate_dependencies(&mut projects, config_path.clone(), &config);

            let mut link_libraries = HashSet::<String>::new();
            let cxx_options = config.cxx_options;
            fn validate_dependencies(projects: &mut HashMap<String, Project>, link_libraries: &mut HashSet<String>, name: &str, root_cxx_options: CxxOptions, root_name: &str) {
                let proj = projects.get(name).unwrap();
                let supported_targets = proj.config.supported_targets.clone();
                if proj.visited {
                    return;
                }

                for dep in proj.dep_names.clone() {
                    validate_dependencies(projects, link_libraries, &dep, root_cxx_options, root_name);
                    let dep = projects.get(&dep).unwrap();
                    if !matches!(dep.config.output_type, OutputType::StaticLibrary) {
                        let dep_type = match dep.config.output_type {
                            OutputType::GuiApp => "GUI app",
                            OutputType::ConsoleApp => "console app",
                            OutputType::DynamicLibrary => "dynamic library",
                            OutputType::StaticLibrary => panic!(),
                        };
                        let proj = projects.get(name).unwrap();
                        fail_immediate!("Project \"{}\" depends on \"{}\", a {}. Only static library dependencies are supported at this time.", proj.config.name, dep.config.name, dep_type);
                    }
                    if !dep.config.cxx_options.is_compatible_with(&root_cxx_options) {
                        fail_immediate!("{}'s C++ options are incompatible with those of the root project \"{}\".", dep.config.name, name);
                    }
                    for platform in &supported_targets {
                        if !dep.config.supported_targets.contains(platform) {
                            fail_immediate!("{} claims to support target {:?}, but its dependency {} does not.", name, platform, dep.config.name);
                        }
                    }
                }
                
                let proj = projects.get_mut(name).unwrap();
                link_libraries.extend(proj.config.link_libraries.iter().cloned());
                proj.visited = true;
            }
            validate_dependencies(&mut projects, &mut link_libraries, &config.name, cxx_options, &config.name);

            async fn build_all<'a>(target: Platform, build_options: &BuildOptions, dependencies: impl IntoIterator<Item=&'a mut Project>, root_project: &mut Project, link_libraries: &[String]) -> (PathBuf, ToolchainPaths) {
                async fn build(target: Platform, build_options: &BuildOptions, config: &ProjectConfig, config_path: &Path) -> (Option<PathBuf>, ToolchainPaths) {
                    let mode = match build_options.compile_mode {
                        CompileMode::Debug => "debug",
                        CompileMode::Release => "release",
                    };
                    println!("Building \"{}\" for target {:?} in {} mode", config.name, target, mode);
    
                    let toolchain_paths = ToolchainPaths::find(target).unwrap();            
                    // Create abs/debug or abs/release, if it doesn't exist already
                    let mut artifact_path: PathBuf = ["abs", mode, &config.name].iter().collect();
                    artifact_path.push(format!("{:?}", target));
        
                    let mut env = BuildEnvironment::new(
                        config,
                        config_path,
                        build_options,
                        &toolchain_paths,
                        // TODO: make these configurable
                        &[("_WINDOWS", ""), ("WIN32", ""), ("UNICODE", ""), ("_USE_MATH_DEFINES", "")],
                        &artifact_path,
                    ).unwrap();
        
                    match env.build().await {
                        Ok(produced_artifact) => {
                            let artifact_path = if produced_artifact {
                                Some(artifact_path)
                            } else {
                                None
                            };
                            (artifact_path, toolchain_paths)
                        }
                        Err(error) => env.fail(error)
                    }
    
                }
                let mut link_libraries = Vec::from(link_libraries);
                for project in dependencies {
                    project.config.adapt_to_workspace(&root_project.config);
                    let (artifact_path, _) = build(target, build_options, &project.config, &project.config_path).await;
                    if let Some(mut artifact_path) = artifact_path {
                        artifact_path.push(format!("{}.lib", project.config.name));
                        link_libraries.push(artifact_path.as_os_str().to_string_lossy().into());
                    }
                    // Add spacing between projects
                    println!();
                }
                root_project.config.link_libraries = link_libraries;
                let (artifact_path, toolchain_paths) = build(target, build_options, &root_project.config, &root_project.config_path).await;
                (artifact_path.unwrap(), toolchain_paths)
            }
            let mut root_project = projects.remove(&config.name).unwrap();
            let mut dependencies: Vec<Project> = projects.into_iter().map(|(_, val)| val).collect();
            let link_libraries: Vec<String> = link_libraries.into_iter().collect();

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
                            build_all(supported_target, build_options, &mut dependencies, &mut root_project, &link_libraries).await;
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
                    let (artifact_path, toolchain_paths) = build_all(target, build_options, &mut dependencies, &mut root_project, &link_libraries).await;
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

                    let (artifact_path, toolchain_paths) = build_all(target, build_options, &mut dependencies, &mut root_project, &link_libraries).await;
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
                OutputType::GuiApp | OutputType::DynamicLibrary | OutputType::StaticLibrary => {}
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
