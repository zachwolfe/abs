use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{self, Write};
use std::cmp::Ordering;
use std::process::Command;
use std::ffi::{OsStr, OsString};
use std::os::windows::prelude::*;
use std::iter;

use super::proj_config::{Host, ProjectConfig, CxxStandard, OutputType};
use super::cmd_options::{BuildOptions, CompileMode};

pub struct ToolchainPaths {
    pub compiler_path: PathBuf,
    pub linker_path: PathBuf,
    pub debugger_path: PathBuf,
    pub include_paths: Vec<PathBuf>,
    pub lib_paths: Vec<PathBuf>,

    pub foundation_contract_path: PathBuf,

    /// The paths to WinMDs included in the downloaded Nuget packages, plus the UnionMetadata directory
    pub winmd_paths: Vec<PathBuf>,

    pub cppwinrt_path: PathBuf,
    pub midl_path: PathBuf,
    pub mdmerge_path: PathBuf,
}

pub struct BuildEnvironment<'a> {
    compiler_flags: Vec<OsString>,
    linker_flags: Vec<OsString>,
    midl_flags: Vec<OsString>,

    toolchain_paths: &'a ToolchainPaths,
    src_dir_path: PathBuf,
    objs_path: PathBuf,
    local_winmds_path: PathBuf,
}

#[derive(Debug)]
pub struct BuildError {
    pub code: Option<i32>,
    pub message: String,
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

impl<'a> BuildEnvironment<'a> {
    pub fn new<'b>(
        host: Host,
        config: &ProjectConfig,
        build_options: &BuildOptions,
        toolchain_paths: &'a ToolchainPaths,
        definitions: impl IntoIterator<Item=&'b [impl AsRef<str> + 'b; 2]>,
        src_dir_path: impl Into<PathBuf>,
        objs_path: impl Into<PathBuf>,
        local_winmds_path: impl Into<PathBuf>,
    ) -> Self {
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
        let linker_flags = match host {
            Host::Windows => {
                let mut flags: Vec<OsString> = vec![
                    "/nologo".into(),
                    "/debug".into(),
                ];
                flags.push(
                    format!(
                        "/SUBSYSTEM:{}",
                        match config.output_type {
                            OutputType::GuiApp => "WINDOWS",
                            OutputType::ConsoleApp => "CONSOLE",
                        },
                    ).into()
                );
                for path in &toolchain_paths.lib_paths {
                    flags.push(cmd_flag("/LIBPATH:", path));
                }
                flags
            }
        };
        let midl_flags = match host {
            Host::Windows => {
                let mut flags = vec![
                    "/winrt".into(),
                    "/metadata_dir".into(),
                    toolchain_paths.foundation_contract_path.as_os_str().to_os_string(),
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
                for winmd_path in &toolchain_paths.winmd_paths {
                    for entry in fs::read_dir(winmd_path).unwrap() {
                        let path = entry.unwrap().path();
                        if path.extension() == Some(OsStr::new("winmd")) {
                            flags.push(OsString::from("/reference"));
                            flags.push(path.as_os_str().to_owned());
                        }
                    }
                }
                for include_path in &toolchain_paths.include_paths {
                    flags.push(OsString::from("/I"));
                    flags.push(include_path.as_os_str().to_owned());
                }
                flags
            },
        };
        BuildEnvironment {
            compiler_flags,
            linker_flags,
            midl_flags,

            toolchain_paths,
            src_dir_path: src_dir_path.into(),
            objs_path: objs_path.into(),
            local_winmds_path: local_winmds_path.into(),
        }
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

    fn compile_idl(&self, path: impl AsRef<Path>, winmd_path: impl AsRef<Path>) {
        let mut flags = self.midl_flags.clone();
        flags.push("/winmd".into());
        flags.push(winmd_path.as_ref().as_os_str().to_os_string());
        flags.push(path.as_ref().as_os_str().to_owned());
        let code = Command::new(&self.toolchain_paths.midl_path)
            .args(flags)
            .env("PATH", self.toolchain_paths.compiler_path.parent().unwrap())
            .spawn()
            .unwrap()
            .wait()
            .unwrap();

        assert!(code.success());
    }

    pub fn compile_idl_directory_recursive(&self, paths: &SrcPaths, winmd_paths: &mut Vec<PathBuf>) {
        for idl_path in &paths.idl_paths {
            let winmd_path = self.get_artifact_path(idl_path, &self.local_winmds_path, "winmd");
            self.compile_idl(idl_path, &winmd_path);
            winmd_paths.push(winmd_path);
        }

        for child in &paths.children {
            self.compile_idl_directory_recursive(child, winmd_paths);
        }
    }

    pub fn compile_idl_directory(&self, paths: &SrcPaths) {
        let mut winmd_paths = Vec::new();
        self.compile_idl_directory_recursive(paths, &mut winmd_paths);
        
        let _unused = fs::remove_dir_all("merged_winmds");
        fs::create_dir_all("merged_winmds").unwrap();
        let mut args = vec![
            "/v".into(),
            "/partial".into(),
            "/o".into(), "merged_winmds".into(),
            "/n:1".into(),
        ];
        for reference in &self.toolchain_paths.winmd_paths {
            args.push("/metadata_dir".into());
            args.push(reference.as_os_str().to_owned());
        }
        for input in winmd_paths {
            args.push("/i".into());
            args.push(input.as_os_str().to_owned());
        }

        let code = Command::new(&self.toolchain_paths.mdmerge_path)
            .args(args)
            .spawn()
            .unwrap()
            .wait()
            .unwrap();
        assert!(code.success());

        let references = self.toolchain_paths.winmd_paths.iter().cloned()
            .chain(iter::once(PathBuf::from("local")));
        fs::create_dir_all("generated_sources").unwrap();
        let mut args = vec![
            OsString::from("-output"), OsString::from("."),
            OsString::from("-component"), OsString::from("generated_sources"),
            OsString::from("-name"), OsString::from("WinUITest"),
            OsString::from("-prefix"),
            OsString::from("-overwrite"),
            OsString::from("-optimize"),
        ];
        for reference in references {
            args.push("-reference".into());
            args.push(reference.as_os_str().to_owned());
        }
        for entry in fs::read_dir("merged_winmds").unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_file() && path.extension() == Some(OsStr::new("winmd")) {
                args.push("-in".into());
                args.push(path.as_os_str().to_owned());
            }
        }
        self.cppwinrt(args);
    }

    fn cppwinrt(&self, args: impl IntoIterator<Item=impl AsRef<OsStr>>) {
        let code = Command::new(&self.toolchain_paths.cppwinrt_path)
            .args(args)
            .spawn()
            .unwrap()
            .wait()
            .unwrap();
        assert!(code.success());
    }

    fn project_winmd(&self, path: impl AsRef<Path>, output_path: impl AsRef<Path>) {
        self.cppwinrt(&[
            OsStr::new("-input"), path.as_ref().as_os_str(),
            OsStr::new("-output"), output_path.as_ref().as_os_str(),
        ]);
    }
    fn project_winmd_with_references(&self, path: impl AsRef<Path>, output_path: impl AsRef<Path>, references: impl IntoIterator<Item=impl AsRef<OsStr>>) {
        let mut args = vec![
            OsString::from("-input"), path.as_ref().as_os_str().to_owned(),
            OsString::from("-output"), output_path.as_ref().as_os_str().to_owned(),
        ];
        for reference in references {
            args.push("-reference".into());
            args.push(reference.as_ref().to_owned());
        }
        self.cppwinrt(args);
    }

    pub fn project_winsdk(&self) {
        self.project_winmd("sdk", "yoyoma");
        for winmd_path in &self.toolchain_paths.winmd_paths {
            self.project_winmd_with_references(winmd_path, "yoyoma", &["local"]);
        }
    }

    fn compile_directory_recursive(&self, paths: &SrcPaths, newest_header: u64, obj_paths: &mut Vec<PathBuf>) -> bool {
        let mut success = true;
        macro_rules! fail {
            ($($t:tt)*) => {
                println!($($t)*);
                success = false;
            }
        }

        fs::create_dir_all(&paths.root).unwrap();
        for path in paths.src_paths.iter() {
            let obj_path = self.get_artifact_path(path, &self.objs_path, "obj");
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
            success &= self.compile_directory_recursive(child, newest_header, obj_paths);
        }

        success
    }


    pub fn compile_directory(
        &self,
        paths: &SrcPaths,
        header_paths: impl IntoIterator<Item=impl AsRef<Path>>,
        obj_paths: &mut Vec<PathBuf>,
    ) -> bool {
        let newest_header = header_paths.into_iter().map(|header| {
            fs::metadata(header).unwrap().last_write_time()
        }).max().unwrap_or(0u64);

        self.compile_directory_recursive(paths, newest_header, obj_paths)
    }

    fn compile(&self, path: impl AsRef<Path>, obj_path: impl AsRef<Path>) -> Result<String, BuildError> {
        let mut args = self.compiler_flags.clone();
        let path = path.as_ref();

        args.push(
            cmd_flag(
                "/Fo",
                self.get_artifact_path(path, &obj_path, "obj")
            )
        );
        args.push(
            cmd_flag(
                "/Fd",
                self.get_artifact_path(path, &obj_path, "pdb")
            )
        );
        args.push("/I".into());
        args.push(".".into());
        args.push("/I".into());
        args.push("yoyoma".into());
        args.push(path.as_os_str().to_owned());
        let output = Command::new(&self.toolchain_paths.compiler_path)
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

    pub fn link(
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
        args.push(
            cmd_flag(
                "/out:",
                output_path,
            )
        );
        for path in obj_paths {
            args.push(path.as_ref().as_os_str().to_owned());
        }
        for path in lib_paths {
            args.push(path.as_ref().as_os_str().to_owned());
        }
        let output = Command::new(&self.toolchain_paths.linker_path)
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
}

fn get_nuget_path() -> &'static Path {
    let path = Path::new(r"abs\vs\nuget.exe");
    if path.is_file() {
        println!("Found nuget.");
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

fn download_nuget_deps(deps: &[&str]) -> Vec<PathBuf> {
    let nuget_path = get_nuget_path();
    let packages_path = nuget_path.parent().unwrap();
    let mut paths = Vec::new();
    for &dep in deps {
        if let Some(existing) = find_nuget_package(dep, packages_path) {
            println!("Found {}.", dep);
            paths.push(existing.clone());
            continue;
        }
        println!("Installing {}...", dep);
        let status = Command::new(nuget_path)
            .args(
                &[
                    "install".into(),
                    "-OutputDirectory".into(),
                    nuget_path.parent().unwrap().to_owned(),
                    dep.into(),
                ]
            )
                .spawn()
                .unwrap()
                .wait()
                .unwrap();
        assert!(status.success());
        paths.push(find_nuget_package(dep, packages_path).unwrap());
    }
    paths
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
    pub fn find() -> io::Result<ToolchainPaths> {
        let dependency_paths = download_nuget_deps(&["Microsoft.Windows.CppWinRT", "Microsoft.ProjectReunion", "Microsoft.ProjectReunion.WinUI", "Microsoft.ProjectReunion.Foundation"]);
        let mut winmd_paths = Vec::new();
        let mut include_paths = Vec::new();
        for md_path in &dependency_paths[2..3] {
            include_paths.push(md_path.join("include"));
            winmd_paths.push(md_path.join(r"lib\uap10.0"));
        }

        let cppwinrt_path = dependency_paths[0].join(r"bin\cppwinrt.exe");

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
        path.push("Hostx64");
        path.push("x64");
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
        for name in &["ucrt", "um"] {
            path.push(name);
            path.push("x64");
            lib_paths.push(path.clone());
            path.pop();
            path.pop();
        }

        let mut path = win10.clone();
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

        let mut path = win10.clone();
        path.push("UnionMetadata");
        path.push(newest_version::<_, 4>(&path).unwrap());
        winmd_paths.push(path.clone());

        let mut path = win10;
        path.push("bin");
        // TODO: error handling
        path.push(newest_version::<_, 4>(&path).unwrap());
        path.push("x64");
        let midl_path = path.join("midl.exe");
        let mdmerge_path = path.join("mdmerge.exe");

        Ok(
            ToolchainPaths {
                compiler_path,
                linker_path,
                debugger_path,
                include_paths,
                lib_paths,

                foundation_contract_path,
                winmd_paths,

                cppwinrt_path,
                midl_path,
                mdmerge_path,
            }
        )
    }
}