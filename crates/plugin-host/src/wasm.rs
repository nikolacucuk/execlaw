//! Restricted H084 trial runtime for deterministic JSON transforms.
//!
//! The module receives no host imports. Its ABI is `alloc(i32) -> i32` and
//! `transform(i32, i32) -> i64`, where the result packs output pointer in the
//! high 32 bits and output length in the low 32 bits. A fresh Store per call
//! prevents guest state from leaking across tool invocations.

use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;
use wasmtime::{Config, Engine, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

const MAX_MODULE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_MEMORY_BYTES: usize = 16 * 1024 * 1024;
const MAX_FUEL: u64 = 10_000_000;
const MAX_WASM_STACK_BYTES: usize = 256 * 1024;
const MAX_PARALLEL_CALLS: usize = 4;
static MODULE_COMPILERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static MODULE_CALLS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone)]
pub struct WasmPlugin {
    engine: Engine,
    module: Module,
}

struct StoreData {
    limits: StoreLimits,
}

impl WasmPlugin {
    /// Compile a module on a blocking worker, with one compile admission at a time.
    pub async fn load_limited(path: PathBuf) -> Result<Self, String> {
        let permit = MODULE_COMPILERS
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Wasm compiler is shutting down".to_owned())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Self::load(&path)
        })
        .await
        .map_err(|error| format!("Wasm compiler worker failed: {error}"))?
    }

    /// Execute on a blocking worker under a small host-wide concurrency limit.
    pub async fn transform_limited(self: Arc<Self>, input: Value) -> Result<Value, String> {
        let permit = MODULE_CALLS
            .get_or_init(|| Arc::new(Semaphore::new(MAX_PARALLEL_CALLS)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Wasm executor is shutting down".to_owned())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            self.transform(&input)
        })
        .await
        .map_err(|error| format!("Wasm transform worker failed: {error}"))?
    }

    /// Load a core Wasm module, rejecting ambient imports before registration.
    pub fn load(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_MODULE_BYTES {
            return Err("Wasm module must be a nonempty file no larger than 32 MiB".into());
        }
        let mut config = Config::new();
        config.consume_fuel(true);
        config.max_wasm_stack(MAX_WASM_STACK_BYTES);
        let engine = Engine::new(&config).map_err(|error| error.to_string())?;
        let module = Module::from_file(&engine, path).map_err(|error| error.to_string())?;
        if let Some(import) = module.imports().next() {
            return Err(format!(
                "Wasm trial modules cannot import host functionality (first import: {}::{})",
                import.module(),
                import.name()
            ));
        }
        Ok(Self { engine, module })
    }

    /// Execute one bounded JSON transformation in a fresh isolated store.
    pub fn transform(&self, input: &Value) -> Result<Value, String> {
        let input = serde_json::to_vec(input).map_err(|error| error.to_string())?;
        if input.len() > MAX_JSON_BYTES {
            return Err("Wasm input JSON exceeds 1 MiB".into());
        }
        let mut store = Store::new(
            &self.engine,
            StoreData {
                limits: StoreLimitsBuilder::new()
                    .memory_size(MAX_MEMORY_BYTES)
                    .table_elements(1024)
                    .memories(1)
                    .tables(1)
                    .instances(1)
                    .build(),
            },
        );
        store.limiter(|data| &mut data.limits);
        store
            .set_fuel(MAX_FUEL)
            .map_err(|error| format!("configure Wasm fuel: {error}"))?;

        // An empty import list makes the no-ambient-capability rule executable.
        let instance = wasmtime::Instance::new(&mut store, &self.module, &[])
            .map_err(|error| format!("instantiate restricted Wasm module: {error}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| "Wasm module must export memory".to_owned())?;
        let alloc: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .map_err(|error| format!("Wasm module must export alloc(i32) -> i32: {error}"))?;
        let transform: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, "transform")
            .map_err(|error| {
                format!("Wasm module must export transform(i32, i32) -> i64: {error}")
            })?;
        let input_len = i32::try_from(input.len()).map_err(|_| "Wasm input is too large")?;
        let input_ptr = alloc
            .call(&mut store, input_len)
            .map_err(|error| format!("Wasm input allocation failed: {error}"))?;
        let input_offset = usize::try_from(input_ptr)
            .map_err(|_| "Wasm allocator returned a negative input pointer")?;
        memory
            .write(&mut store, input_offset, &input)
            .map_err(|error| format!("Wasm input buffer is out of bounds: {error}"))?;
        let packed = transform
            .call(&mut store, (input_ptr, input_len))
            .map_err(|error| format!("Wasm transform trapped or exhausted fuel: {error}"))?
            as u64;
        let output_ptr =
            usize::try_from((packed >> 32) as u32).map_err(|_| "Wasm output pointer is invalid")?;
        let output_len = (packed & u64::from(u32::MAX)) as usize;
        if output_len > MAX_JSON_BYTES {
            return Err("Wasm output JSON exceeds 1 MiB".into());
        }
        let mut output = vec![0; output_len];
        memory
            .read(&store, output_ptr, &mut output)
            .map_err(|error| format!("Wasm output buffer is out of bounds: {error}"))?;
        serde_json::from_slice(&output)
            .map_err(|error| format!("Wasm output is not valid JSON: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(contents: &str) -> tempfile::NamedTempFile {
        let bytes = wat::parse_str(contents).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        file
    }

    #[test]
    fn json_transform_uses_the_typed_contract() {
        let file = module(
            r#"(module
                (memory (export "memory") 1 1)
                (data (i32.const 0) "{}")
                (func (export "alloc") (param i32) (result i32) (i32.const 1024))
                (func (export "transform") (param i32 i32) (result i64)
                    (i64.const 2)))"#,
        );
        let plugin = WasmPlugin::load(file.path()).unwrap();
        assert_eq!(
            plugin.transform(&serde_json::json!({"x":1})).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn imports_are_rejected_before_a_module_can_be_registered() {
        let file = module(
            r#"(module
                (import "wasi_snapshot_preview1" "fd_write" (func))
                (memory (export "memory") 1 1))"#,
        );
        assert!(
            matches!(WasmPlugin::load(file.path()), Err(error) if error.contains("cannot import"))
        );
    }

    #[test]
    fn infinite_loop_exhausts_fuel_without_starving_the_host() {
        let file = module(
            r#"(module
                (memory (export "memory") 1 1)
                (func (export "alloc") (param i32) (result i32) (i32.const 0))
                (func (export "transform") (param i32 i32) (result i64)
                    (loop $again (br $again))
                    (i64.const 0)))"#,
        );
        let plugin = WasmPlugin::load(file.path()).unwrap();
        assert!(
            plugin
                .transform(&serde_json::json!({}))
                .unwrap_err()
                .contains("fuel")
        );
    }

    #[test]
    fn linear_memory_is_limited() {
        let file = module(
            r#"(module
                (memory (export "memory") 257 257)
                (func (export "alloc") (param i32) (result i32) (i32.const 0))
                (func (export "transform") (param i32 i32) (result i64) (i64.const 0)))"#,
        );
        let plugin = WasmPlugin::load(file.path()).unwrap();
        assert!(
            plugin
                .transform(&serde_json::json!({}))
                .unwrap_err()
                .contains("memory")
        );
    }

    #[test]
    fn repeated_calls_do_not_reuse_guest_memory() {
        let file = module(
            r#"(module
                (memory (export "memory") 1 1)
                (data (i32.const 0) "{}")
                (func (export "alloc") (param i32) (result i32) (i32.const 1024))
                (func (export "transform") (param i32 i32) (result i64)
                    (i64.const 2)))"#,
        );
        let plugin = WasmPlugin::load(file.path()).unwrap();
        assert_eq!(
            plugin.transform(&serde_json::json!(1)).unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            plugin.transform(&serde_json::json!(2)).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn json_input_and_output_are_bounded() {
        let input_module = module(
            r#"(module
                (memory (export "memory") 1 1)
                (data (i32.const 0) "{}")
                (func (export "alloc") (param i32) (result i32) (i32.const 1024))
                (func (export "transform") (param i32 i32) (result i64) (i64.const 2)))"#,
        );
        let input_plugin = WasmPlugin::load(input_module.path()).unwrap();
        assert!(
            input_plugin
                .transform(&serde_json::json!("x".repeat(MAX_JSON_BYTES)))
                .unwrap_err()
                .contains("input JSON")
        );

        let output_module = module(
            r#"(module
                (memory (export "memory") 1 1)
                (func (export "alloc") (param i32) (result i32) (i32.const 0))
                (func (export "transform") (param i32 i32) (result i64) (i64.const 1048577)))"#,
        );
        let output_plugin = WasmPlugin::load(output_module.path()).unwrap();
        assert!(
            output_plugin
                .transform(&serde_json::json!({}))
                .unwrap_err()
                .contains("output JSON")
        );
    }
}
