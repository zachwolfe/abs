use std::path::{PathBuf, Path};
use std::io::Error as IoError;
use std::time::SystemTime;
use std::ffi::OsString;
use std::cmp::Ordering;
use std::fs;

use crate::Platform;
use crate::proj_config::Arch;

pub struct ToolchainPaths {
    pub debugger_path: PathBuf,
    pub include_paths: Vec<PathBuf>,
    pub lib_paths: Vec<PathBuf>,
    pub bin_paths: Vec<PathBuf>,
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
    pub fn find(target: Platform) -> Result<ToolchainPaths, IoError> {
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
