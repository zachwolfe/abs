use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task;
use indicatif::ProgressBar;

// TODO: should not depend on BuildEnvironment
use crate::build::{WarningCache, BuildEnvironment, BuildError, PchOption, DependencyBuilder};
use crate::cmd_options::CompileMode;
use crate::proj_config::{Platform, Os};
use crate::build_manager::{compile_cxx, CompileFlags, CompilerOutput};
use crate::println_above_progress_bar_if_visible;

#[async_trait]
pub trait Task {
    fn previous_valid_run(&self, env: &BuildEnvironment) -> Result<Option<PathBuf>, BuildError>;
    async fn run_guaranteed(&self, env: &BuildEnvironment) -> Result<PathBuf, BuildError>;
}

#[async_trait]
pub trait TaskExt: Task {
    async fn run(&self, env: &BuildEnvironment) -> Result<PathBuf, BuildError>;
}
#[async_trait]
impl<T: Task + Sync> TaskExt for T {
    async fn run(&self, env: &BuildEnvironment) -> Result<PathBuf, BuildError> {
        if let Some(prev) = self.previous_valid_run(env)? {
            Ok(prev)
        } else {
            self.run_guaranteed(env).await
        }
    }
}

pub struct IdentityTask(PathBuf);

#[async_trait]
impl Task for IdentityTask {
    fn previous_valid_run(&self, _env: &BuildEnvironment) -> Result<Option<PathBuf>, BuildError> {
        Ok(Some(self.0.clone()))
    }

    async fn run_guaranteed(&self, _env: &BuildEnvironment) -> Result<PathBuf, BuildError> {
        Ok(self.0.clone())
    }
}

pub struct CxxTask { src: Box<dyn TaskExt + Sync + Send>, pch: PchOption }

impl CxxTask {
    pub fn compile(src: impl Into<PathBuf>, pch: PchOption) -> Self {
        Self { src: Box::new(IdentityTask(src.into())), pch }
    }
}

#[async_trait]
impl Task for CxxTask {
    fn previous_valid_run(&self, env: &BuildEnvironment) -> Result<Option<PathBuf>, BuildError> {
        let path = self.src.previous_valid_run(env)?;
        let path = if let Some(path) = path {
            path
        } else {
            return Ok(None)
        };
        let generating_pch = matches!(self.pch, PchOption::GeneratePch);
        let extension = if generating_pch {
            "pch"
        } else {
            "obj"
        };
        let artifact_path = env.get_artifact_path(&path, &env.objs_path, extension);
        let is_pch = path.file_name() == Some(OsStr::new("pch.cpp")) && path.parent() == Some(&env.src_dir_path);
        let dependencies = env.discover_src_deps(&path)?.map(|dependencies| {
            DependencyBuilder::default()
                .file(path)
                .files(dependencies)
                .build()
        });

        let should_rebuild = (generating_pch || !is_pch) && if let Some(dependencies) = &dependencies {
            env.should_build_artifact(dependencies, &artifact_path)?
        } else {
            true
        };

        if should_rebuild {
            Ok(None)
        } else {
            Ok(Some(artifact_path))
        }
    }

    async fn run_guaranteed(&self, env: &BuildEnvironment) -> Result<PathBuf, BuildError> {
        let path = self.src.run(env).await?;
        let host = Platform::host();
        let obj_path = env.objs_path.clone();
        // TODO: instead of matching over the host OS here, perhaps it would be better to match over the compiler vendor
        let (flags, obj_path) = match host.os() {
            Os::Windows => {
                let mut flags = CompileFlags::empty()
                    .singles([
                        "/W3",
                        "/Zi",
                        "/EHsc",
                        "/c",
                        "/FS",
                    ])
                    .rtti(env.config.cxx_options.rtti)
                    .async_await(env.config.cxx_options.async_await)
                    .cxx_standard(env.config.cxx_options.standard);

                match env.build_options.compile_mode {
                    CompileMode::Debug => flags = flags.singles(["/MDd", "/RTC1"]),
                    CompileMode::Release => flags = flags.single("/O2"),
                }
                flags = flags
                    .defines(env.definitions.iter().cloned())
                    .include_paths(&env.toolchain_paths.include_paths)
                    .include_paths([
                        &env.dependency_headers_path,
                        &env.src_dir_path,
                    ]);
                match self.pch {
                    PchOption::GeneratePch | PchOption::UsePch => {
                        let path = env.get_artifact_path(env.src_dir_path.join("pch.h"), &obj_path, "pch");
                        flags = flags.pch_path(path, matches!(self.pch, PchOption::GeneratePch));
                    },
                    _ => {}
                }
                let src_deps_json_path = env.get_artifact_path(&path, &env.src_deps_path, "json");
                let src_deps_parent = src_deps_json_path.parent().unwrap();
                fs::create_dir_all(src_deps_parent)?;
                let obj_path = env.get_artifact_path(&path, &obj_path, "obj");
                flags = flags
                    .obj_path(&obj_path)
                    .double("/Fd", env.objs_path.join(&format!("{}.pdb", &env.config.name)))
                    .double("/sourceDependencies", src_deps_json_path)
                    .src_path(&path);
                (flags, obj_path)
            },
            Os::Linux => {
                // TODO: implement
                (CompileFlags::empty(), env.get_artifact_path(&path, &obj_path, "o"))
            }
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<CompilerOutput>();
        let unique_output = env.unique_compiler_output.clone();
        let progress_bar = env.progress_bar.lock().unwrap().clone();
        let handle = task::spawn(async move {
            // let unique_output = ;
            let mut warning_cache = WarningCache::default();
            while let Some(output) = rx.recv().await {
                match &output {
                    CompilerOutput::Begun { .. } => {},
                    CompilerOutput::Error(s) | CompilerOutput::Warning(s) => {
                        if unique_output.lock().unwrap().insert(s.lines().next().unwrap().to_string()) {
                            println_above_progress_bar_if_visible!(progress_bar, "{}", s);
                        }
                        if matches!(output, CompilerOutput::Warning(_)) {
                            warning_cache.warnings.push(s.clone());
                        }
                    }
                }
            }
            warning_cache
        });

        let val = if compile_cxx(&env.toolchain_paths, flags, tx).await {
            Ok(obj_path)
        } else {
            Err(BuildError::CompilerError)
        };
        if let Some(progress_bar) = env.progress_bar.lock().unwrap().upgrade() {
            progress_bar.inc(1);
        }
        let warning_cache = handle.await.unwrap();
        let warning_cache_path = env.get_artifact_path(path, &env.warning_cache_path, "warnings");
        if let Some(parent) = warning_cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let warning_cache = serde_json::to_string(&warning_cache).unwrap();
        fs::write(warning_cache_path, warning_cache)?;
        val
    }
}

/*
fn build() {
    let task = CxxTask::compile("hello.cpp");
    task.run();
}

*/