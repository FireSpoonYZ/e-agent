mod buffer_shim;
mod crypto_shim;
mod embedded_assets;
mod error;
mod extensions;
mod hostcall_io_uring_lane;
mod hostcall_queue;
mod hostcall_s3_fifo;
mod http_shim;
pub mod runtime;
pub mod scheduler;
mod tools;

pub use error::{Error, Result};
pub use runtime::{
    ExtensionToolDef, HostcallKind, HostcallRequest, NativeCall, NativeCallFuture, NativeFunction,
    NativeModule, PiJsRuntime, PiJsRuntimeConfig, ProgramOutput,
};
pub use scheduler::{HostcallOutcome, WallClock};

/// Run one JavaScript/TypeScript module with native extension modules.
pub async fn execute_program(
    source: &str,
    modules: &[NativeModule],
    call: NativeCall,
) -> Result<ProgramOutput> {
    let runtime = PiJsRuntime::new().await?;
    runtime.install_native_modules(modules, call).await?;
    runtime.execute_program(source).await
}

/// Run one module and dispatch hostcalls while its top-level Promise is pending.
pub async fn execute_program_with_hostcalls<F, Fut>(
    source: &str,
    modules: &[NativeModule],
    call: NativeCall,
    dispatch: F,
) -> Result<ProgramOutput>
where
    F: FnMut(HostcallRequest) -> Fut,
    Fut: std::future::Future<Output = Vec<HostcallOutcome>>,
{
    let runtime = PiJsRuntime::new().await?;
    runtime.install_native_modules(modules, call).await?;
    runtime
        .execute_program_with_hostcalls(source, dispatch)
        .await
}

/// Run JavaScript in a fresh Node-compatible QuickJS runtime.
pub async fn run(source: &str) -> Result<serde_json::Value> {
    let runtime = PiJsRuntime::new().await?;
    runtime.eval(source).await?;
    runtime.drain_microtasks().await?;
    runtime.read_global_json("__e_agent_result").await
}

/// Run a JavaScript or TypeScript module and return its default export as JSON.
pub async fn run_file(path: impl AsRef<std::path::Path>) -> Result<serde_json::Value> {
    let path = extensions::safe_canonicalize(path.as_ref());
    let mut config = PiJsRuntimeConfig::default();
    config.cwd = path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_string_lossy()
        .into_owned();
    let runtime = PiJsRuntime::with_clock_and_config(scheduler::WallClock, config).await?;
    runtime.add_extension_root(
        path.parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf(),
    );
    let specifier = path.to_string_lossy().replace('\\', "/");
    runtime
        .eval(&format!(
            "import({specifier:?}).then(m => globalThis.__e_agent_result = m.default)"
        ))
        .await?;
    runtime.drain_microtasks().await?;
    runtime.read_global_json("__e_agent_result").await
}
