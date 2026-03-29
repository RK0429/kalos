use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use wasmtime::{
    Caller, Config, Engine, Extern, Instance, Linker, Module, Mutability, Store, StoreLimits,
    StoreLimitsBuilder, Trap, Val,
};

use crate::domains::config::{PluginModuleRef, ResolvedPluginManifest};
use crate::domains::cpg::{CpgSubgraph, EdgeKind, NodeKind};
use crate::domains::metrics::{
    MetricConfig, MetricDefinition, MetricOrigin, MetricParticipation, MetricValue, round_half_up,
};
use crate::domains::{AnalysisLevel, MetricId, RuleId, ScopeId};
use crate::ports::{PluginEvaluationRequest, PluginPort};

pub const SPI_VERSION: &str = "kalos-metric-spi-v1";
pub const PER_INVOCATION_FUEL_BUDGET: u64 = 500_000;
pub const FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET: u64 = 30_000_000;
pub const DIFF_ANALYSIS_AGGREGATE_FUEL_BUDGET: u64 = 5_000_000;
pub const LINEAR_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginMetricDefinition {
    id: MetricId,
    name: String,
    level: AnalysisLevel,
    description: String,
}

impl PluginMetricDefinition {
    fn new(
        id: impl Into<MetricId>,
        level: AnalysisLevel,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            level,
            description: description.into(),
        }
    }
}

impl MetricDefinition for PluginMetricDefinition {
    fn id(&self) -> &MetricId {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn level(&self) -> AnalysisLevel {
        self.level
    }

    fn origin(&self) -> MetricOrigin {
        MetricOrigin::Plugin
    }

    fn participation(&self) -> MetricParticipation {
        MetricParticipation::ReportOnly
    }

    fn rule_binding(&self) -> Option<&RuleId> {
        None
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn compute(&self, _subgraph: &CpgSubgraph, _config: &MetricConfig) -> Option<MetricValue> {
        None
    }
}

#[derive(Debug, Error)]
pub enum PluginHostError {
    #[error("failed to read plugin module `{path}`: {source}")]
    ReadModule {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("plugin module `{path}` checksum mismatch: expected `{expected}`, actual `{actual}`")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("plugin module `{path}` is missing custom section `kalos_spi_version`")]
    SpiVersionMissing { path: PathBuf },
    #[error(
        "plugin module `{path}` SPI version mismatch: expected `{expected}`, actual `{actual}`"
    )]
    SpiVersionMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("failed to compile plugin module `{path}`: {source}")]
    WasmCompilation {
        path: PathBuf,
        #[source]
        source: wasmtime::Error,
    },
    #[error("failed to instantiate plugin module `{path}`: {source}")]
    WasmInstantiation {
        path: PathBuf,
        #[source]
        source: wasmtime::Error,
    },
    #[error("plugin module `{path}` init returned non-zero status `{code}`")]
    InitFailed { path: PathBuf, code: i32 },
    #[error("plugin module `{path}` trapped during init: {source}")]
    InitTrapped {
        path: PathBuf,
        #[source]
        source: wasmtime::Error,
    },
    #[error("plugin module `{path}` attempted to register duplicate metric id `{metric_id}`")]
    MetricIdCollision { path: PathBuf, metric_id: String },
    #[error("plugin module `{path}` is missing or has an invalid export `{export}`: {source}")]
    RequiredExport {
        path: PathBuf,
        export: &'static str,
        #[source]
        source: wasmtime::Error,
    },
    #[error("plugin module `{path}` does not export linear memory as `memory`")]
    MissingMemoryExport { path: PathBuf },
    #[error(
        "plugin module `{path}` attempted to read invalid guest memory range ptr={pointer} len={len}"
    )]
    MemoryRead {
        path: PathBuf,
        pointer: u32,
        len: u32,
    },
    #[error(
        "plugin module `{path}` attempted to write invalid guest memory range ptr={pointer} len={len}"
    )]
    MemoryWrite {
        path: PathBuf,
        pointer: u32,
        len: u32,
    },
    #[error("plugin module `{path}` provided invalid UTF-8 for `{field}`")]
    InvalidGuestString { path: PathBuf, field: &'static str },
    #[error("plugin module `{path}` attempted to register unsupported analysis level `{level}`")]
    InvalidMetricLevel { path: PathBuf, level: u32 },
    #[error("plugin module `{path}` provided invalid ScopeId encoding")]
    InvalidScopeEncoding { path: PathBuf },
    #[error(
        "plugin module `{path}` attempted to use host export `{host_export}` outside evaluation"
    )]
    MissingEvaluationContext {
        path: PathBuf,
        host_export: &'static str,
    },
    #[error("failed to restore guest state for plugin module `{path}`: {detail}")]
    GuestStateRestore { path: PathBuf, detail: String },
    #[error("failed to configure fuel for plugin module `{path}`: {source}")]
    FuelControl {
        path: PathBuf,
        #[source]
        source: wasmtime::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleLoadWarningKind {
    ReadModule,
    ChecksumMismatch,
    SpiVersionMissing,
    SpiVersionMismatch,
    WasmCompilation,
    WasmInstantiation,
    InitFailed,
    InitTrapped,
    MetricIdCollision,
    RequiredExport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleLoadWarning {
    pub path: PathBuf,
    pub kind: ModuleLoadWarningKind,
    pub message: String,
}

impl ModuleLoadWarning {
    fn from_error(error: &PluginHostError) -> Self {
        let (path, kind) = match error {
            PluginHostError::ReadModule { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::ReadModule)
            }
            PluginHostError::ChecksumMismatch { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::ChecksumMismatch)
            }
            PluginHostError::SpiVersionMissing { path } => {
                (path.clone(), ModuleLoadWarningKind::SpiVersionMissing)
            }
            PluginHostError::SpiVersionMismatch { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::SpiVersionMismatch)
            }
            PluginHostError::WasmCompilation { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::WasmCompilation)
            }
            PluginHostError::WasmInstantiation { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::WasmInstantiation)
            }
            PluginHostError::InitFailed { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::InitFailed)
            }
            PluginHostError::InitTrapped { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::InitTrapped)
            }
            PluginHostError::MetricIdCollision { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::MetricIdCollision)
            }
            PluginHostError::RequiredExport { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::RequiredExport)
            }
            PluginHostError::MissingMemoryExport { path }
            | PluginHostError::MemoryRead { path, .. }
            | PluginHostError::MemoryWrite { path, .. }
            | PluginHostError::InvalidGuestString { path, .. }
            | PluginHostError::InvalidMetricLevel { path, .. }
            | PluginHostError::InvalidScopeEncoding { path }
            | PluginHostError::MissingEvaluationContext { path, .. }
            | PluginHostError::GuestStateRestore { path, .. }
            | PluginHostError::FuelControl { path, .. } => {
                (path.clone(), ModuleLoadWarningKind::InitTrapped)
            }
        };

        Self {
            path,
            kind,
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluationWarningKind {
    UnknownMetric,
    AggregateFuelExhausted,
    PerInvocationFuelExhausted,
    MemoryLimitExceeded,
    Trap,
    InvalidRawValue,
    InvalidNormalizedRisk,
    ClampedNormalizedRisk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationWarning {
    pub path: PathBuf,
    pub kind: EvaluationWarningKind,
    pub metric_id: Option<MetricId>,
    pub scope_id: Option<ScopeId>,
    pub message: String,
}

pub struct WasmPluginHost {
    definitions: Vec<PluginMetricDefinition>,
    warnings: Vec<ModuleLoadWarning>,
    evaluation_warnings: Vec<EvaluationWarning>,
    loaded_modules: Vec<LoadedModule>,
    metric_to_module: BTreeMap<MetricId, usize>,
    aggregate_fuel_remaining: u64,
}

impl WasmPluginHost {
    pub fn load(
        workspace_root: &Path,
        manifest: &ResolvedPluginManifest,
        existing_metric_ids: &BTreeSet<MetricId>,
        aggregate_fuel_budget: u64,
    ) -> Self {
        let mut definitions = Vec::new();
        let mut warnings = Vec::new();
        let mut loaded_modules = Vec::new();
        let mut metric_to_module = BTreeMap::new();
        let mut known_metric_ids = existing_metric_ids.clone();

        for module_ref in &manifest.modules {
            let module_path = workspace_root.join(module_ref.workspace_relative_path.as_str());
            match load_module(&module_path, module_ref, &known_metric_ids) {
                Ok((module_definitions, loaded_module)) => {
                    let module_index = loaded_modules.len();
                    known_metric_ids.extend(
                        module_definitions
                            .iter()
                            .map(|definition| definition.id.clone()),
                    );
                    for definition in &module_definitions {
                        metric_to_module.insert(definition.id.clone(), module_index);
                    }
                    definitions.extend(module_definitions);
                    loaded_modules.push(loaded_module);
                }
                Err(error) => {
                    tracing::warn!(path = %module_path.display(), error = %error, "failed to load plugin module");
                    warnings.push(ModuleLoadWarning::from_error(&error));
                }
            }
        }

        Self {
            definitions,
            warnings,
            evaluation_warnings: Vec::new(),
            loaded_modules,
            metric_to_module,
            aggregate_fuel_remaining: aggregate_fuel_budget,
        }
    }

    pub fn warnings(&self) -> &[ModuleLoadWarning] {
        &self.warnings
    }

    pub fn evaluation_warnings(&self) -> &[EvaluationWarning] {
        &self.evaluation_warnings
    }

    pub fn module_count(&self) -> usize {
        self.loaded_modules.len()
    }
}

impl PluginPort for WasmPluginHost {
    type Error = PluginHostError;

    fn load_metric_definitions(&self) -> Result<Vec<Box<dyn MetricDefinition>>, Self::Error> {
        Ok(self
            .definitions
            .iter()
            .cloned()
            .map(|definition| Box::new(definition) as Box<dyn MetricDefinition>)
            .collect())
    }

    fn reset_aggregate_fuel_budget(&mut self, budget: u64) {
        self.aggregate_fuel_remaining = budget;
    }

    fn evaluate(
        &mut self,
        definition: &dyn MetricDefinition,
        request: &PluginEvaluationRequest,
    ) -> Result<Option<MetricValue>, Self::Error> {
        let metric_id = definition.id().clone();
        let scope_id = request.scope_id.clone();

        let Some(&module_index) = self.metric_to_module.get(&metric_id) else {
            self.evaluation_warnings.push(build_evaluation_warning(
                PathBuf::from("<unknown>"),
                EvaluationWarningKind::UnknownMetric,
                Some(metric_id.clone()),
                Some(scope_id.clone()),
                format!("plugin metric `{metric_id}` is not associated with any loaded module"),
            ));
            return Ok(None);
        };

        if self.aggregate_fuel_remaining == 0 {
            let module_path = self.loaded_modules[module_index].module_path.clone();
            self.evaluation_warnings.push(build_evaluation_warning(
                module_path.clone(),
                EvaluationWarningKind::AggregateFuelExhausted,
                Some(metric_id.clone()),
                Some(scope_id.clone()),
                format!(
                    "skipping plugin metric `{metric_id}` for scope `{}:{}` because aggregate fuel budget is exhausted",
                    scope_id.qualified_name,
                    scope_id.file_path
                ),
            ));
            tracing::warn!(
                path = %module_path.display(),
                metric_id = %metric_id,
                scope = %scope_id.qualified_name,
                "aggregate plugin fuel budget exhausted"
            );
            return Ok(None);
        }

        let invocation_fuel = PER_INVOCATION_FUEL_BUDGET.min(self.aggregate_fuel_remaining);
        let invocation_result;
        let remaining_fuel;
        {
            let loaded_module = &mut self.loaded_modules[module_index];
            restore_init_snapshot(loaded_module)?;
            loaded_module
                .store
                .set_fuel(invocation_fuel)
                .map_err(|source| PluginHostError::FuelControl {
                    path: loaded_module.module_path.clone(),
                    source,
                })?;

            loaded_module.store.data_mut().evaluation_context =
                Some(build_evaluation_context(request));
            invocation_result = run_evaluation(loaded_module, &metric_id, &scope_id)?;
            remaining_fuel =
                loaded_module
                    .store
                    .get_fuel()
                    .map_err(|source| PluginHostError::FuelControl {
                        path: loaded_module.module_path.clone(),
                        source,
                    })?;
            loaded_module.store.data_mut().evaluation_context = None;
        }

        let consumed_fuel = invocation_fuel.saturating_sub(remaining_fuel);
        self.aggregate_fuel_remaining = self.aggregate_fuel_remaining.saturating_sub(consumed_fuel);

        for warning in invocation_result.warnings {
            tracing::warn!(
                path = %warning.path.display(),
                metric_id = warning.metric_id.as_ref().map(|value| value.as_str()),
                scope = warning.scope_id.as_ref().map(|value| value.qualified_name.as_str()),
                warning = %warning.message,
                "plugin evaluation warning"
            );
            self.evaluation_warnings.push(warning);
        }

        Ok(invocation_result.metric_value)
    }
}

struct LoadedModule {
    store: Store<ModuleStoreData>,
    instance: Instance,
    module_path: PathBuf,
    init_snapshot: InitSnapshot,
}

struct InitSnapshot {
    memory_data: Vec<u8>,
    memory_byte_size: usize,
    globals: Vec<(String, Val)>,
}

struct ModuleStoreData {
    path: PathBuf,
    known_metric_ids: BTreeSet<MetricId>,
    pending_definitions: Vec<PluginMetricDefinition>,
    collision_metric_id: Option<MetricId>,
    evaluation_context: Option<EvaluationContext>,
    limits: StoreLimits,
}

impl ModuleStoreData {
    fn new(path: PathBuf, known_metric_ids: BTreeSet<MetricId>) -> Self {
        Self {
            path,
            known_metric_ids,
            pending_definitions: Vec::new(),
            collision_metric_id: None,
            evaluation_context: None,
            limits: StoreLimitsBuilder::new()
                .memory_size(LINEAR_MEMORY_LIMIT_BYTES)
                .build(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EvaluationContext {
    filtered_nodes: Vec<FilteredNode>,
    filtered_edges: Vec<FilteredEdge>,
    config_entries: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilteredNode {
    kind_discriminant: u32,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilteredEdge {
    source_index: u32,
    target_index: u32,
    kind_discriminant: u32,
}

struct RegisterMetricRequest {
    id_ptr: u32,
    id_len: u32,
    level: u32,
    name_ptr: u32,
    name_len: u32,
    desc_ptr: u32,
    desc_len: u32,
}

struct InvocationResult {
    metric_value: Option<MetricValue>,
    warnings: Vec<EvaluationWarning>,
}

fn load_module(
    module_path: &Path,
    module_ref: &PluginModuleRef,
    known_metric_ids: &BTreeSet<MetricId>,
) -> Result<(Vec<PluginMetricDefinition>, LoadedModule), PluginHostError> {
    let bytes = fs::read(module_path).map_err(|source| PluginHostError::ReadModule {
        path: module_path.to_path_buf(),
        source,
    })?;

    let actual_checksum = sha256_hex(&bytes);
    if actual_checksum != module_ref.sha256 {
        return Err(PluginHostError::ChecksumMismatch {
            path: module_path.to_path_buf(),
            expected: module_ref.sha256.clone(),
            actual: actual_checksum,
        });
    }

    verify_spi_version(module_path, &bytes)?;

    let engine = build_engine(module_path)?;
    let module =
        Module::new(&engine, &bytes).map_err(|source| PluginHostError::WasmCompilation {
            path: module_path.to_path_buf(),
            source,
        })?;

    let mut linker = Linker::new(&engine);
    define_host_imports(module_path, &module, &mut linker)?;

    let mut store = Store::new(
        &engine,
        ModuleStoreData::new(module_path.to_path_buf(), known_metric_ids.clone()),
    );
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(PER_INVOCATION_FUEL_BUDGET)
        .map_err(|source| PluginHostError::FuelControl {
            path: module_path.to_path_buf(),
            source,
        })?;

    let instance = linker.instantiate(&mut store, &module).map_err(|source| {
        PluginHostError::WasmInstantiation {
            path: module_path.to_path_buf(),
            source,
        }
    })?;

    let init = instance
        .get_typed_func::<(), i32>(&mut store, "kalos_plugin_init")
        .map_err(|source| PluginHostError::RequiredExport {
            path: module_path.to_path_buf(),
            export: "kalos_plugin_init",
            source,
        })?;
    instance
        .get_typed_func::<(u32, u32, u32, u32), i64>(&mut store, "kalos_plugin_evaluate")
        .map_err(|source| PluginHostError::RequiredExport {
            path: module_path.to_path_buf(),
            export: "kalos_plugin_evaluate",
            source,
        })?;
    instance
        .get_typed_func::<u32, u32>(&mut store, "kalos_plugin_alloc")
        .map_err(|source| PluginHostError::RequiredExport {
            path: module_path.to_path_buf(),
            export: "kalos_plugin_alloc",
            source,
        })?;
    instance
        .get_typed_func::<(u32, u32), ()>(&mut store, "kalos_plugin_free")
        .map_err(|source| PluginHostError::RequiredExport {
            path: module_path.to_path_buf(),
            export: "kalos_plugin_free",
            source,
        })?;

    let init_result = init
        .call(&mut store, ())
        .map_err(|source| PluginHostError::InitTrapped {
            path: module_path.to_path_buf(),
            source,
        })?;

    if let Some(metric_id) = store.data().collision_metric_id.clone() {
        return Err(PluginHostError::MetricIdCollision {
            path: module_path.to_path_buf(),
            metric_id: metric_id.to_string(),
        });
    }

    if init_result != 0 {
        return Err(PluginHostError::InitFailed {
            path: module_path.to_path_buf(),
            code: init_result,
        });
    }

    let init_snapshot = capture_init_snapshot(module_path, &mut store, &instance)?;
    let definitions = mem::take(&mut store.data_mut().pending_definitions);
    Ok((
        definitions,
        LoadedModule {
            store,
            instance,
            module_path: module_path.to_path_buf(),
            init_snapshot,
        },
    ))
}

fn build_engine(module_path: &Path) -> Result<Engine, PluginHostError> {
    let mut config = Config::new();
    config.consume_fuel(true);
    Engine::new(&config).map_err(|source| PluginHostError::WasmCompilation {
        path: module_path.to_path_buf(),
        source,
    })
}

fn define_host_imports(
    module_path: &Path,
    module: &Module,
    linker: &mut Linker<ModuleStoreData>,
) -> Result<(), PluginHostError> {
    let namespaces = module
        .imports()
        .map(|import| import.module().to_owned())
        .collect::<BTreeSet<_>>();

    for namespace in namespaces {
        linker
            .func_wrap(
                &namespace,
                "metric_register",
                |mut caller: Caller<'_, ModuleStoreData>,
                 id_ptr: u32,
                 id_len: u32,
                 level: u32,
                 name_ptr: u32,
                 name_len: u32,
                 desc_ptr: u32,
                 desc_len: u32|
                 -> wasmtime::Result<i32> {
                    register_metric(
                        &mut caller,
                        RegisterMetricRequest {
                            id_ptr,
                            id_len,
                            level,
                            name_ptr,
                            name_len,
                            desc_ptr,
                            desc_len,
                        },
                    )
                },
            )
            .map_err(|source| PluginHostError::WasmInstantiation {
                path: module_path.to_path_buf(),
                source,
            })?;

        linker
            .func_wrap(
                &namespace,
                "cpg_node_count",
                |mut caller: Caller<'_, ModuleStoreData>, scope_ptr: u32, scope_len: u32| {
                    cpg_node_count(&mut caller, scope_ptr, scope_len)
                },
            )
            .map_err(|source| PluginHostError::WasmInstantiation {
                path: module_path.to_path_buf(),
                source,
            })?;

        linker
            .func_wrap(
                &namespace,
                "cpg_edge_count",
                |mut caller: Caller<'_, ModuleStoreData>, scope_ptr: u32, scope_len: u32| {
                    cpg_edge_count(&mut caller, scope_ptr, scope_len)
                },
            )
            .map_err(|source| PluginHostError::WasmInstantiation {
                path: module_path.to_path_buf(),
                source,
            })?;

        linker
            .func_wrap(
                &namespace,
                "cpg_read_node",
                |mut caller: Caller<'_, ModuleStoreData>,
                 scope_ptr: u32,
                 scope_len: u32,
                 index: u32,
                 buf_ptr: u32,
                 buf_len: u32|
                 -> wasmtime::Result<i32> {
                    cpg_read_node(&mut caller, scope_ptr, scope_len, index, buf_ptr, buf_len)
                },
            )
            .map_err(|source| PluginHostError::WasmInstantiation {
                path: module_path.to_path_buf(),
                source,
            })?;

        linker
            .func_wrap(
                &namespace,
                "cpg_read_edge",
                |mut caller: Caller<'_, ModuleStoreData>,
                 scope_ptr: u32,
                 scope_len: u32,
                 index: u32,
                 buf_ptr: u32,
                 buf_len: u32|
                 -> wasmtime::Result<i32> {
                    cpg_read_edge(&mut caller, scope_ptr, scope_len, index, buf_ptr, buf_len)
                },
            )
            .map_err(|source| PluginHostError::WasmInstantiation {
                path: module_path.to_path_buf(),
                source,
            })?;

        linker
            .func_wrap(
                &namespace,
                "config_read",
                |mut caller: Caller<'_, ModuleStoreData>,
                 key_ptr: u32,
                 key_len: u32,
                 buf_ptr: u32,
                 buf_len: u32|
                 -> wasmtime::Result<i32> {
                    config_read(&mut caller, key_ptr, key_len, buf_ptr, buf_len)
                },
            )
            .map_err(|source| PluginHostError::WasmInstantiation {
                path: module_path.to_path_buf(),
                source,
            })?;
    }

    Ok(())
}

fn register_metric(
    caller: &mut Caller<'_, ModuleStoreData>,
    request: RegisterMetricRequest,
) -> wasmtime::Result<i32> {
    let id = read_guest_string(caller, "id", request.id_ptr, request.id_len)?;
    let name = read_guest_string(caller, "name", request.name_ptr, request.name_len)?;
    let description = read_guest_string(caller, "description", request.desc_ptr, request.desc_len)?;
    let level = decode_analysis_level(caller.data().path.clone(), request.level)?;
    let metric_id = MetricId::from(id);

    let state = caller.data_mut();
    if state.known_metric_ids.contains(&metric_id) {
        state.collision_metric_id.get_or_insert(metric_id);
        return Ok(-1);
    }

    state.known_metric_ids.insert(metric_id.clone());
    state.pending_definitions.push(PluginMetricDefinition::new(
        metric_id,
        level,
        name,
        description,
    ));
    Ok(0)
}

fn cpg_node_count(
    caller: &mut Caller<'_, ModuleStoreData>,
    scope_ptr: u32,
    scope_len: u32,
) -> wasmtime::Result<u32> {
    validate_scope_id(caller, scope_ptr, scope_len)?;
    let count = caller
        .data()
        .evaluation_context
        .as_ref()
        .ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingEvaluationContext {
                    path: caller.data().path.clone(),
                    host_export: "cpg_node_count",
                }
                .to_string(),
            )
        })?
        .filtered_nodes
        .len();
    Ok(usize_to_u32_saturating(count))
}

fn cpg_edge_count(
    caller: &mut Caller<'_, ModuleStoreData>,
    scope_ptr: u32,
    scope_len: u32,
) -> wasmtime::Result<u32> {
    validate_scope_id(caller, scope_ptr, scope_len)?;
    let count = caller
        .data()
        .evaluation_context
        .as_ref()
        .ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingEvaluationContext {
                    path: caller.data().path.clone(),
                    host_export: "cpg_edge_count",
                }
                .to_string(),
            )
        })?
        .filtered_edges
        .len();
    Ok(usize_to_u32_saturating(count))
}

fn cpg_read_node(
    caller: &mut Caller<'_, ModuleStoreData>,
    scope_ptr: u32,
    scope_len: u32,
    index: u32,
    buf_ptr: u32,
    buf_len: u32,
) -> wasmtime::Result<i32> {
    validate_scope_id(caller, scope_ptr, scope_len)?;
    let encoded = {
        let context = caller.data().evaluation_context.as_ref().ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingEvaluationContext {
                    path: caller.data().path.clone(),
                    host_export: "cpg_read_node",
                }
                .to_string(),
            )
        })?;
        let Some(node) = context.filtered_nodes.get(index as usize) else {
            return Ok(-2);
        };
        encode_filtered_node(node)
    };

    if usize::try_from(buf_len).unwrap_or(usize::MAX) < encoded.len() {
        return Ok(-1);
    }

    write_guest_bytes(caller, buf_ptr, &encoded)?;
    Ok(i32::try_from(encoded.len()).unwrap_or(i32::MAX))
}

fn cpg_read_edge(
    caller: &mut Caller<'_, ModuleStoreData>,
    scope_ptr: u32,
    scope_len: u32,
    index: u32,
    buf_ptr: u32,
    buf_len: u32,
) -> wasmtime::Result<i32> {
    validate_scope_id(caller, scope_ptr, scope_len)?;
    let encoded = {
        let context = caller.data().evaluation_context.as_ref().ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingEvaluationContext {
                    path: caller.data().path.clone(),
                    host_export: "cpg_read_edge",
                }
                .to_string(),
            )
        })?;
        let Some(edge) = context.filtered_edges.get(index as usize) else {
            return Ok(-2);
        };
        encode_filtered_edge(edge)
    };

    if buf_len < encoded.len() as u32 {
        return Ok(-1);
    }

    write_guest_bytes(caller, buf_ptr, &encoded)?;
    Ok(i32::try_from(encoded.len()).unwrap_or(i32::MAX))
}

fn config_read(
    caller: &mut Caller<'_, ModuleStoreData>,
    key_ptr: u32,
    key_len: u32,
    buf_ptr: u32,
    buf_len: u32,
) -> wasmtime::Result<i32> {
    let key = read_guest_string(caller, "key", key_ptr, key_len)?;
    let value = caller
        .data()
        .evaluation_context
        .as_ref()
        .ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingEvaluationContext {
                    path: caller.data().path.clone(),
                    host_export: "config_read",
                }
                .to_string(),
            )
        })?
        .config_entries
        .get(&key)
        .cloned();

    let Some(value) = value else {
        return Ok(-2);
    };
    let value_bytes = value.into_bytes();
    if usize::try_from(buf_len).unwrap_or(usize::MAX) < value_bytes.len() {
        return Ok(-1);
    }

    write_guest_bytes(caller, buf_ptr, &value_bytes)?;
    Ok(i32::try_from(value_bytes.len()).unwrap_or(i32::MAX))
}

fn run_evaluation(
    loaded_module: &mut LoadedModule,
    metric_id: &MetricId,
    scope_id: &ScopeId,
) -> Result<InvocationResult, PluginHostError> {
    let metric_bytes = metric_id.as_str().as_bytes();
    let scope_bytes = encode_scope_id(scope_id);

    let metric_allocation = match allocate_guest_bytes(loaded_module, metric_bytes) {
        Ok(allocation) => allocation,
        Err(error) => {
            return Ok(InvocationResult {
                metric_value: None,
                warnings: vec![classify_evaluation_error(
                    &loaded_module.module_path,
                    metric_id,
                    scope_id,
                    error,
                )],
            });
        }
    };
    let scope_allocation = match allocate_guest_bytes(loaded_module, &scope_bytes) {
        Ok(allocation) => allocation,
        Err(error) => {
            return Ok(InvocationResult {
                metric_value: None,
                warnings: vec![classify_evaluation_error(
                    &loaded_module.module_path,
                    metric_id,
                    scope_id,
                    error,
                )],
            });
        }
    };

    let evaluate = loaded_module
        .instance
        .get_typed_func::<(u32, u32, u32, u32), i64>(
            &mut loaded_module.store,
            "kalos_plugin_evaluate",
        )
        .map_err(|source| PluginHostError::RequiredExport {
            path: loaded_module.module_path.clone(),
            export: "kalos_plugin_evaluate",
            source,
        })?;

    match evaluate.call(
        &mut loaded_module.store,
        (
            metric_allocation.0,
            metric_allocation.1,
            scope_allocation.0,
            scope_allocation.1,
        ),
    ) {
        Ok(encoded) => {
            let mut warnings = Vec::new();
            let metric_value = decode_metric_value(
                &loaded_module.module_path,
                metric_id,
                scope_id,
                encoded,
                &mut warnings,
            );
            Ok(InvocationResult {
                metric_value,
                warnings,
            })
        }
        Err(error) => Ok(InvocationResult {
            metric_value: None,
            warnings: vec![classify_evaluation_error(
                &loaded_module.module_path,
                metric_id,
                scope_id,
                error,
            )],
        }),
    }
}

fn allocate_guest_bytes(
    loaded_module: &mut LoadedModule,
    bytes: &[u8],
) -> Result<(u32, u32), wasmtime::Error> {
    let alloc = loaded_module
        .instance
        .get_typed_func::<u32, u32>(&mut loaded_module.store, "kalos_plugin_alloc")
        .map_err(|source| {
            wasmtime::Error::msg(
                PluginHostError::RequiredExport {
                    path: loaded_module.module_path.clone(),
                    export: "kalos_plugin_alloc",
                    source,
                }
                .to_string(),
            )
        })?;
    let len = u32::try_from(bytes.len()).map_err(|_| {
        wasmtime::Error::msg(
            PluginHostError::MemoryWrite {
                path: loaded_module.module_path.clone(),
                pointer: 0,
                len: u32::MAX,
            }
            .to_string(),
        )
    })?;
    let ptr = alloc.call(&mut loaded_module.store, len)?;
    write_guest_bytes_to_store(&mut loaded_module.store, loaded_module.instance, ptr, bytes)
        .map_err(wasmtime::Error::msg)?;
    Ok((ptr, len))
}

fn capture_init_snapshot(
    module_path: &Path,
    store: &mut Store<ModuleStoreData>,
    instance: &Instance,
) -> Result<InitSnapshot, PluginHostError> {
    let memory = instance.get_memory(&mut *store, "memory").ok_or_else(|| {
        PluginHostError::MissingMemoryExport {
            path: module_path.to_path_buf(),
        }
    })?;

    let memory_data = memory.data(&mut *store).to_vec();
    let memory_byte_size = memory.data_size(&mut *store);
    let exports = instance
        .exports(&mut *store)
        .map(|export| (export.name().to_owned(), export.into_extern()))
        .collect::<Vec<_>>();
    let mut globals = Vec::new();
    for (name, export) in exports {
        let Some(global) = export.into_global() else {
            continue;
        };
        if global.ty(&mut *store).mutability() == Mutability::Var {
            globals.push((name, global.get(&mut *store)));
        }
    }

    Ok(InitSnapshot {
        memory_data,
        memory_byte_size,
        globals,
    })
}

fn restore_init_snapshot(loaded_module: &mut LoadedModule) -> Result<(), PluginHostError> {
    let memory = loaded_module
        .instance
        .get_memory(&mut loaded_module.store, "memory")
        .ok_or_else(|| PluginHostError::MissingMemoryExport {
            path: loaded_module.module_path.clone(),
        })?;

    let current_size = memory.data_size(&mut loaded_module.store);
    if current_size < loaded_module.init_snapshot.memory_byte_size {
        return Err(PluginHostError::GuestStateRestore {
            path: loaded_module.module_path.clone(),
            detail: format!(
                "current memory shrank below init snapshot (current={current_size}, snapshot={})",
                loaded_module.init_snapshot.memory_byte_size
            ),
        });
    }

    {
        let data = memory.data_mut(&mut loaded_module.store);
        data[..loaded_module.init_snapshot.memory_byte_size]
            .copy_from_slice(&loaded_module.init_snapshot.memory_data);
        if current_size > loaded_module.init_snapshot.memory_byte_size {
            data[loaded_module.init_snapshot.memory_byte_size..].fill(0);
        }
    }

    for (name, value) in &loaded_module.init_snapshot.globals {
        let Some(global) = loaded_module
            .instance
            .get_global(&mut loaded_module.store, name)
        else {
            return Err(PluginHostError::GuestStateRestore {
                path: loaded_module.module_path.clone(),
                detail: format!("mutable exported global `{name}` disappeared"),
            });
        };
        global
            .set(&mut loaded_module.store, *value)
            .map_err(|error| PluginHostError::GuestStateRestore {
                path: loaded_module.module_path.clone(),
                detail: format!("failed to restore global `{name}`: {error}"),
            })?;
    }

    Ok(())
}

fn build_evaluation_context(request: &PluginEvaluationRequest) -> EvaluationContext {
    let mut node_indices = BTreeMap::new();
    let mut filtered_nodes = Vec::new();
    for node in &request.subgraph.nodes {
        let Some(kind_discriminant) = node_kind_discriminant(node.kind) else {
            continue;
        };
        let index = usize_to_u32_saturating(filtered_nodes.len());
        node_indices.insert(node.id, index);
        filtered_nodes.push(FilteredNode {
            kind_discriminant,
            name: node.name.clone(),
        });
    }

    let mut filtered_edges = Vec::new();
    for edge in &request.subgraph.edges {
        let Some(kind_discriminant) = edge_kind_discriminant(edge.kind) else {
            continue;
        };
        let (Some(&source_index), Some(&target_index)) = (
            node_indices.get(&edge.source),
            node_indices.get(&edge.target),
        ) else {
            continue;
        };
        filtered_edges.push(FilteredEdge {
            source_index,
            target_index,
            kind_discriminant,
        });
    }

    EvaluationContext {
        filtered_nodes,
        filtered_edges,
        config_entries: request.config.entries.clone(),
    }
}

fn node_kind_discriminant(kind: NodeKind) -> Option<u32> {
    let discriminant = match kind {
        NodeKind::Function => 0,
        NodeKind::Class => 1,
        NodeKind::Module => 2,
        NodeKind::Variable => 3,
        NodeKind::Parameter => 4,
        NodeKind::ExternalSymbol => 5,
    };
    Some(discriminant)
}

fn edge_kind_discriminant(kind: EdgeKind) -> Option<u32> {
    let discriminant = match kind {
        EdgeKind::Call => 0,
        EdgeKind::DataFlow => 1,
        EdgeKind::ControlFlow => 2,
        EdgeKind::Contains => 3,
        EdgeKind::TypeReference => 4,
        EdgeKind::Semantic => 5,
    };
    Some(discriminant)
}

fn decode_metric_value(
    module_path: &Path,
    metric_id: &MetricId,
    scope_id: &ScopeId,
    encoded: i64,
    warnings: &mut Vec<EvaluationWarning>,
) -> Option<MetricValue> {
    let bits = encoded as u64;
    let raw_value = f32::from_bits(bits as u32) as f64;
    let normalized_risk = f32::from_bits((bits >> 32) as u32) as f64;

    if !raw_value.is_finite() {
        warnings.push(build_evaluation_warning(
            module_path.to_path_buf(),
            EvaluationWarningKind::InvalidRawValue,
            Some(metric_id.clone()),
            Some(scope_id.clone()),
            format!("plugin metric `{metric_id}` returned non-finite raw_value `{raw_value}`"),
        ));
        return None;
    }

    if !normalized_risk.is_finite() {
        warnings.push(build_evaluation_warning(
            module_path.to_path_buf(),
            EvaluationWarningKind::InvalidNormalizedRisk,
            Some(metric_id.clone()),
            Some(scope_id.clone()),
            format!(
                "plugin metric `{metric_id}` returned non-finite normalized_risk `{normalized_risk}`"
            ),
        ));
        return None;
    }

    let clamped_risk = normalized_risk.clamp(0.0, 1.0);
    if normalized_risk != clamped_risk {
        warnings.push(build_evaluation_warning(
            module_path.to_path_buf(),
            EvaluationWarningKind::ClampedNormalizedRisk,
            Some(metric_id.clone()),
            Some(scope_id.clone()),
            format!(
                "plugin metric `{metric_id}` returned normalized_risk `{normalized_risk}` outside [0, 1]; clamped to `{clamped_risk}`"
            ),
        ));
    }

    Some(MetricValue {
        metric_id: metric_id.clone(),
        raw_value: round_half_up(raw_value, 6),
        normalized_risk: round_half_up(clamped_risk, 6),
    })
}

fn classify_evaluation_error(
    module_path: &Path,
    metric_id: &MetricId,
    scope_id: &ScopeId,
    error: wasmtime::Error,
) -> EvaluationWarning {
    let (kind, message) = match error.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => (
            EvaluationWarningKind::PerInvocationFuelExhausted,
            format!(
                "plugin metric `{metric_id}` exceeded per-invocation fuel budget ({PER_INVOCATION_FUEL_BUDGET})"
            ),
        ),
        Some(Trap::AllocationTooLarge) => (
            EvaluationWarningKind::MemoryLimitExceeded,
            format!(
                "plugin metric `{metric_id}` exceeded linear memory limit ({LINEAR_MEMORY_LIMIT_BYTES} bytes)"
            ),
        ),
        _ => (
            EvaluationWarningKind::Trap,
            format!("plugin metric `{metric_id}` trapped during evaluation: {error}"),
        ),
    };

    build_evaluation_warning(
        module_path.to_path_buf(),
        kind,
        Some(metric_id.clone()),
        Some(scope_id.clone()),
        message,
    )
}

fn build_evaluation_warning(
    path: PathBuf,
    kind: EvaluationWarningKind,
    metric_id: Option<MetricId>,
    scope_id: Option<ScopeId>,
    message: String,
) -> EvaluationWarning {
    EvaluationWarning {
        path,
        kind,
        metric_id,
        scope_id,
        message,
    }
}

fn encode_scope_id(scope_id: &ScopeId) -> Vec<u8> {
    let qualified_name = scope_id.qualified_name.as_bytes();
    let file_path = scope_id.file_path.as_str().as_bytes();
    let mut bytes = Vec::with_capacity(12 + qualified_name.len() + file_path.len());
    push_u32_le(encode_analysis_level(scope_id.level), &mut bytes);
    push_u32_le(usize_to_u32_saturating(qualified_name.len()), &mut bytes);
    bytes.extend_from_slice(qualified_name);
    push_u32_le(usize_to_u32_saturating(file_path.len()), &mut bytes);
    bytes.extend_from_slice(file_path);
    bytes
}

fn decode_scope_id(bytes: &[u8], path: PathBuf) -> Result<ScopeId, PluginHostError> {
    let mut cursor = 0_usize;
    let raw_level = read_u32_le(bytes, &mut cursor)
        .ok_or_else(|| PluginHostError::InvalidScopeEncoding { path: path.clone() })?;
    let qualified_name_len = read_u32_le(bytes, &mut cursor)
        .ok_or_else(|| PluginHostError::InvalidScopeEncoding { path: path.clone() })?
        as usize;
    let qualified_name_end = cursor
        .checked_add(qualified_name_len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| PluginHostError::InvalidScopeEncoding { path: path.clone() })?;
    let qualified_name = std::str::from_utf8(&bytes[cursor..qualified_name_end]).map_err(|_| {
        PluginHostError::InvalidGuestString {
            path: path.clone(),
            field: "scope.qualified_name",
        }
    })?;
    cursor = qualified_name_end;

    let file_path_len = read_u32_le(bytes, &mut cursor)
        .ok_or_else(|| PluginHostError::InvalidScopeEncoding { path: path.clone() })?
        as usize;
    let file_path_end = cursor
        .checked_add(file_path_len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| PluginHostError::InvalidScopeEncoding { path: path.clone() })?;
    let file_path = std::str::from_utf8(&bytes[cursor..file_path_end]).map_err(|_| {
        PluginHostError::InvalidGuestString {
            path: path.clone(),
            field: "scope.file_path",
        }
    })?;
    cursor = file_path_end;

    if cursor != bytes.len() {
        return Err(PluginHostError::InvalidScopeEncoding { path });
    }

    Ok(ScopeId::new(
        decode_analysis_level(path, raw_level)?,
        qualified_name.to_owned(),
        file_path,
    ))
}

fn push_u32_le(value: u32, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u32_le(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    let end = cursor.checked_add(4)?;
    let raw = bytes.get(*cursor..end)?;
    *cursor = end;
    Some(u32::from_le_bytes(raw.try_into().ok()?))
}

fn encode_analysis_level(level: AnalysisLevel) -> u32 {
    match level {
        AnalysisLevel::Function => 0,
        AnalysisLevel::Module => 1,
        AnalysisLevel::Project => 2,
    }
}

fn encode_filtered_node(node: &FilteredNode) -> Vec<u8> {
    let name_bytes = node.name.as_bytes();
    let mut encoded = Vec::with_capacity(8 + name_bytes.len());
    push_u32_le(node.kind_discriminant, &mut encoded);
    push_u32_le(usize_to_u32_saturating(name_bytes.len()), &mut encoded);
    encoded.extend_from_slice(name_bytes);
    encoded
}

fn encode_filtered_edge(edge: &FilteredEdge) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(12);
    push_u32_le(edge.source_index, &mut encoded);
    push_u32_le(edge.target_index, &mut encoded);
    push_u32_le(edge.kind_discriminant, &mut encoded);
    encoded
}

fn validate_scope_id(
    caller: &mut Caller<'_, ModuleStoreData>,
    scope_ptr: u32,
    scope_len: u32,
) -> wasmtime::Result<()> {
    let bytes = read_guest_bytes(caller, scope_ptr, scope_len)?;
    decode_scope_id(&bytes, caller.data().path.clone())
        .map(|_| ())
        .map_err(|error| wasmtime::Error::msg(error.to_string()))
}

fn read_guest_bytes(
    caller: &mut Caller<'_, ModuleStoreData>,
    ptr: u32,
    len: u32,
) -> wasmtime::Result<Vec<u8>> {
    let path = caller.data().path.clone();
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingMemoryExport { path: path.clone() }.to_string(),
            )
        })?;

    let offset = usize::try_from(ptr).map_err(|_| {
        wasmtime::Error::msg(
            PluginHostError::MemoryRead {
                path: path.clone(),
                pointer: ptr,
                len,
            }
            .to_string(),
        )
    })?;
    let length = usize::try_from(len).map_err(|_| {
        wasmtime::Error::msg(
            PluginHostError::MemoryRead {
                path: path.clone(),
                pointer: ptr,
                len,
            }
            .to_string(),
        )
    })?;
    let mut bytes = vec![0_u8; length];
    memory.read(caller, offset, &mut bytes).map_err(|_| {
        wasmtime::Error::msg(
            PluginHostError::MemoryRead {
                path: path.clone(),
                pointer: ptr,
                len,
            }
            .to_string(),
        )
    })?;

    Ok(bytes)
}

fn read_guest_string(
    caller: &mut Caller<'_, ModuleStoreData>,
    field: &'static str,
    ptr: u32,
    len: u32,
) -> wasmtime::Result<String> {
    let bytes = read_guest_bytes(caller, ptr, len)?;
    String::from_utf8(bytes).map_err(|_| {
        wasmtime::Error::msg(
            PluginHostError::InvalidGuestString {
                path: caller.data().path.clone(),
                field,
            }
            .to_string(),
        )
    })
}

fn write_guest_bytes(
    caller: &mut Caller<'_, ModuleStoreData>,
    ptr: u32,
    bytes: &[u8],
) -> wasmtime::Result<()> {
    let path = caller.data().path.clone();
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| {
            wasmtime::Error::msg(
                PluginHostError::MissingMemoryExport { path: path.clone() }.to_string(),
            )
        })?;
    memory
        .write(caller, usize::try_from(ptr).unwrap_or(usize::MAX), bytes)
        .map_err(|_| {
            wasmtime::Error::msg(
                PluginHostError::MemoryWrite {
                    path,
                    pointer: ptr,
                    len: usize_to_u32_saturating(bytes.len()),
                }
                .to_string(),
            )
        })
}

fn write_guest_bytes_to_store(
    store: &mut Store<ModuleStoreData>,
    instance: Instance,
    ptr: u32,
    bytes: &[u8],
) -> Result<(), String> {
    let path = store.data().path.clone();
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| PluginHostError::MissingMemoryExport { path: path.clone() }.to_string())?;
    memory
        .write(
            &mut *store,
            usize::try_from(ptr).unwrap_or(usize::MAX),
            bytes,
        )
        .map_err(|_| {
            PluginHostError::MemoryWrite {
                path,
                pointer: ptr,
                len: usize_to_u32_saturating(bytes.len()),
            }
            .to_string()
        })
}

fn decode_analysis_level(path: PathBuf, raw_level: u32) -> Result<AnalysisLevel, PluginHostError> {
    match raw_level {
        0 => Ok(AnalysisLevel::Function),
        1 => Ok(AnalysisLevel::Module),
        2 => Ok(AnalysisLevel::Project),
        _ => Err(PluginHostError::InvalidMetricLevel {
            path,
            level: raw_level,
        }),
    }
}

fn verify_spi_version(module_path: &Path, bytes: &[u8]) -> Result<(), PluginHostError> {
    let Some(actual) = find_custom_section(bytes, "kalos_spi_version") else {
        return Err(PluginHostError::SpiVersionMissing {
            path: module_path.to_path_buf(),
        });
    };

    if actual == SPI_VERSION.as_bytes() {
        return Ok(());
    }

    Err(PluginHostError::SpiVersionMismatch {
        path: module_path.to_path_buf(),
        expected: SPI_VERSION.to_owned(),
        actual: String::from_utf8_lossy(actual).into_owned(),
    })
}

fn find_custom_section<'a>(bytes: &'a [u8], section_name: &str) -> Option<&'a [u8]> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" || bytes[4..8] != [1, 0, 0, 0] {
        return None;
    }

    let mut cursor = 8_usize;
    while cursor < bytes.len() {
        let section_id = *bytes.get(cursor)?;
        cursor += 1;

        let (section_len, size_len) = read_var_u32(&bytes[cursor..])?;
        cursor += size_len;
        let section_end = cursor.checked_add(usize::try_from(section_len).ok()?)?;
        if section_end > bytes.len() {
            return None;
        }

        if section_id == 0 {
            let (name_len, name_len_size) = read_var_u32(&bytes[cursor..section_end])?;
            let name_start = cursor + name_len_size;
            let name_end = name_start.checked_add(usize::try_from(name_len).ok()?)?;
            if name_end > section_end {
                return None;
            }

            if &bytes[name_start..name_end] == section_name.as_bytes() {
                return Some(&bytes[name_end..section_end]);
            }
        }

        cursor = section_end;
    }

    None
}

fn read_var_u32(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut result = 0_u32;
    let mut shift = 0_u32;

    for (index, byte) in bytes.iter().copied().enumerate() {
        let value = u32::from(byte & 0x7f);
        result |= value.checked_shl(shift)?;
        if (byte & 0x80) == 0 {
            return Some((result, index + 1));
        }
        shift += 7;
        if shift >= 35 {
            return None;
        }
    }

    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    value.min(u32::MAX as usize) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::*;
    use crate::domains::cpg::{CpgEdge, CpgNode, EdgeKind, NodeId, NodeKind, SourceLocation};
    use crate::domains::{FilePath, ScopeId};

    #[test]
    fn load_valid_plugin_registers_metric_and_evaluates() {
        let workspace = tempdir().unwrap();
        let wasm = build_test_module(
            Some(SPI_VERSION),
            0,
            &[RegistrationSpec {
                id: "plugin.metric",
                level: 1,
                name: "Plugin Metric",
                description: "Registered from wasm",
            }],
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);

        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.warnings().is_empty());
        assert_eq!(host.module_count(), 1);

        let definitions = host.load_metric_definitions().unwrap();
        assert_eq!(definitions.len(), 1);
        let definition = &definitions[0];
        assert_eq!(definition.id().as_str(), "plugin.metric");
        assert_eq!(definition.name(), "Plugin Metric");
        assert_eq!(definition.description(), "Registered from wasm");
        assert_eq!(definition.level(), AnalysisLevel::Module);
        assert_eq!(definition.origin(), MetricOrigin::Plugin);
        assert_eq!(definition.participation(), MetricParticipation::ReportOnly);
        assert_eq!(definition.rule_binding(), None);

        let request = sample_request();
        assert_eq!(
            host.evaluate(definition.as_ref(), &request).unwrap(),
            Some(MetricValue {
                metric_id: MetricId::from("plugin.metric"),
                raw_value: 0.0,
                normalized_risk: 0.0,
            })
        );
        assert!(host.evaluation_warnings().is_empty());
        assert_eq!(definition.compute(&request.subgraph, &request.config), None);
    }

    #[test]
    fn load_checksum_mismatch_skips_module() {
        let workspace = tempdir().unwrap();
        let wasm = build_test_module(
            Some(SPI_VERSION),
            0,
            &[sample_registration("plugin.metric")],
        );
        write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = ResolvedPluginManifest {
            modules: vec![PluginModuleRef {
                workspace_relative_path: FilePath::from("plugin.wasm"),
                sha256: "0".repeat(64),
            }],
        };

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert_eq!(host.module_count(), 0);
        assert_eq!(host.warnings().len(), 1);
        assert_eq!(
            host.warnings()[0].kind,
            ModuleLoadWarningKind::ChecksumMismatch
        );
    }

    #[test]
    fn load_spi_version_missing_skips_module() {
        let workspace = tempdir().unwrap();
        let wasm = build_test_module(None, 0, &[sample_registration("plugin.metric")]);
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert_eq!(host.module_count(), 0);
        assert_eq!(
            host.warnings()[0].kind,
            ModuleLoadWarningKind::SpiVersionMissing
        );
    }

    #[test]
    fn load_spi_version_mismatch_skips_module() {
        let workspace = tempdir().unwrap();
        let wasm = build_test_module(
            Some("kalos-metric-spi-v0"),
            0,
            &[sample_registration("plugin.metric")],
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert_eq!(host.module_count(), 0);
        assert_eq!(
            host.warnings()[0].kind,
            ModuleLoadWarningKind::SpiVersionMismatch
        );
    }

    #[test]
    fn load_init_failure_rolls_back_all_registrations() {
        let workspace = tempdir().unwrap();
        let wasm = build_test_module(
            Some(SPI_VERSION),
            7,
            &[sample_registration("plugin.metric")],
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert_eq!(host.module_count(), 0);
        assert_eq!(host.warnings()[0].kind, ModuleLoadWarningKind::InitFailed);
    }

    #[test]
    fn load_metric_id_collision_rolls_back_module() {
        let workspace = tempdir().unwrap();
        let wasm = build_test_module(
            Some(SPI_VERSION),
            0,
            &[
                sample_registration("plugin.unique"),
                sample_registration("builtin.metric"),
            ],
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let existing_metric_ids = BTreeSet::from([MetricId::from("builtin.metric")]);

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &existing_metric_ids,
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert_eq!(host.module_count(), 0);
        assert_eq!(
            host.warnings()[0].kind,
            ModuleLoadWarningKind::MetricIdCollision
        );
    }

    #[test]
    fn load_multiple_plugins_preserves_manifest_order() {
        let workspace = tempdir().unwrap();
        let first = build_test_module(Some(SPI_VERSION), 0, &[sample_registration("alpha.metric")]);
        let second = build_test_module(Some(SPI_VERSION), 0, &[sample_registration("beta.metric")]);
        let first_checksum = write_plugin(workspace.path(), "a.wasm", &first);
        let second_checksum = write_plugin(workspace.path(), "b.wasm", &second);
        let manifest = ResolvedPluginManifest {
            modules: vec![
                PluginModuleRef {
                    workspace_relative_path: FilePath::from("a.wasm"),
                    sha256: first_checksum,
                },
                PluginModuleRef {
                    workspace_relative_path: FilePath::from("b.wasm"),
                    sha256: second_checksum,
                },
            ],
        };

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definitions = host.load_metric_definitions().unwrap();
        let metric_ids = definitions
            .iter()
            .map(|definition| definition.id().as_str().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(metric_ids, vec!["alpha.metric", "beta.metric"]);
        assert!(host.warnings().is_empty());
        assert_eq!(host.module_count(), 2);
    }

    #[test]
    fn load_empty_manifest_returns_empty() {
        let workspace = tempdir().unwrap();
        let host = WasmPluginHost::load(
            workspace.path(),
            &ResolvedPluginManifest::default(),
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert!(host.warnings().is_empty());
        assert_eq!(host.module_count(), 0);
    }

    #[test]
    fn load_missing_file_skips_module() {
        let workspace = tempdir().unwrap();
        let manifest = ResolvedPluginManifest {
            modules: vec![PluginModuleRef {
                workspace_relative_path: FilePath::from("missing.wasm"),
                sha256: "f".repeat(64),
            }],
        };

        let host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );

        assert!(host.load_metric_definitions().unwrap().is_empty());
        assert_eq!(host.module_count(), 0);
        assert_eq!(host.warnings()[0].kind, ModuleLoadWarningKind::ReadModule);
    }

    #[test]
    fn scope_id_binary_layout_round_trips() {
        let scope = ScopeId::new(AnalysisLevel::Function, "crate::alpha", "src/lib.rs");
        let encoded = encode_scope_id(&scope);

        let mut expected = Vec::new();
        expected.extend_from_slice(&0_u32.to_le_bytes());
        expected.extend_from_slice(&(scope.qualified_name.len() as u32).to_le_bytes());
        expected.extend_from_slice(scope.qualified_name.as_bytes());
        expected.extend_from_slice(&(scope.file_path.as_str().len() as u32).to_le_bytes());
        expected.extend_from_slice(scope.file_path.as_str().as_bytes());

        assert_eq!(encoded, expected);
        assert_eq!(
            decode_scope_id(&encoded, PathBuf::from("plugin.wasm")).unwrap(),
            scope
        );
    }

    #[test]
    fn scope_id_decode_rejects_trailing_bytes() {
        let mut encoded = encode_scope_id(&ScopeId::new(AnalysisLevel::Project, "<project>", "."));
        encoded.push(0);

        assert!(matches!(
            decode_scope_id(&encoded, PathBuf::from("plugin.wasm")),
            Err(PluginHostError::InvalidScopeEncoding { .. })
        ));
    }

    #[test]
    fn evaluate_uses_cpg_count_host_exports() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module(
            r#"
    (local $nodes i32)
    (local $edges i32)
    local.get 2
    local.get 3
    call $cpg_node_count
    local.set $nodes
    local.get 2
    local.get 3
    call $cpg_edge_count
    local.set $edges
    f32.const 0.25
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.const 32
    i64.shl
    local.get $nodes
    local.get $edges
    i32.add
    f32.convert_i32_u
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.or
            "#,
            "",
            "",
            1,
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let value = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(value.raw_value, 3.0);
        assert_eq!(value.normalized_risk, 0.25);
    }

    #[test]
    fn evaluate_uses_cpg_read_host_exports() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module(
            r#"
    (local $len0 i32)
    (local $len1 i32)
    (local $edge_len i32)
    local.get 2
    local.get 3
    i32.const 0
    i32.const 600
    i32.const 32
    call $cpg_read_node
    local.set $len0
    local.get 2
    local.get 3
    i32.const 1
    i32.const 600
    i32.const 32
    call $cpg_read_node
    local.set $len1
    local.get 2
    local.get 3
    i32.const 0
    i32.const 700
    i32.const 12
    call $cpg_read_edge
    local.set $edge_len
    f32.const 0.5
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.const 32
    i64.shl
    local.get $len0
    i32.const 600
    i32.load offset=4
    i32.add
    local.get $len1
    i32.add
    i32.const 700
    i32.load offset=8
    i32.add
    local.get $edge_len
    i32.add
    f32.convert_i32_u
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.or
            "#,
            "",
            "",
            1,
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let value = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(value.raw_value, 44.0);
        assert_eq!(value.normalized_risk, 0.5);
    }

    #[test]
    fn evaluate_uses_config_read_host_export() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module(
            r#"
    (local $len i32)
    i32.const 256
    i32.const 9
    i32.const 600
    i32.const 16
    call $config_read
    local.set $len
    f32.const 0.125
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.const 32
    i64.shl
    local.get $len
    i32.const 600
    i32.load8_u
    i32.add
    f32.convert_i32_u
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.or
            "#,
            r#"  (data (i32.const 256) "threshold")"#,
            "",
            1,
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let value = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(value.raw_value, 53.0);
        assert_eq!(value.normalized_risk, 0.125);
    }

    #[test]
    fn evaluate_decodes_scalar_and_rounds_half_up() {
        let workspace = tempdir().unwrap();
        let encoded = encode_metric_bits(1.2345676, 0.5000006);
        let wasm = build_evaluation_module(&format!("i64.const {encoded}"), "", "", 1);
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let value = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(value.raw_value, 1.234568);
        assert_eq!(value.normalized_risk, 0.500001);
        assert!(host.evaluation_warnings().is_empty());
    }

    #[test]
    fn evaluate_rejects_nan_raw_value() {
        let workspace = tempdir().unwrap();
        let encoded = encode_metric_bits(f32::NAN, 0.25);
        let wasm = build_evaluation_module(&format!("i64.const {encoded}"), "", "", 1);
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        assert_eq!(
            host.evaluate(definition.as_ref(), &sample_request())
                .unwrap(),
            None
        );
        assert_eq!(
            host.evaluation_warnings()[0].kind,
            EvaluationWarningKind::InvalidRawValue
        );
    }

    #[test]
    fn evaluate_rejects_infinite_normalized_risk() {
        let workspace = tempdir().unwrap();
        let encoded = encode_metric_bits(1.0, f32::INFINITY);
        let wasm = build_evaluation_module(&format!("i64.const {encoded}"), "", "", 1);
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        assert_eq!(
            host.evaluate(definition.as_ref(), &sample_request())
                .unwrap(),
            None
        );
        assert_eq!(
            host.evaluation_warnings()[0].kind,
            EvaluationWarningKind::InvalidNormalizedRisk
        );
    }

    #[test]
    fn evaluate_clamps_out_of_range_normalized_risk() {
        let workspace = tempdir().unwrap();
        let encoded = encode_metric_bits(2.0, 1.25);
        let wasm = build_evaluation_module(&format!("i64.const {encoded}"), "", "", 1);
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let value = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(value.raw_value, 2.0);
        assert_eq!(value.normalized_risk, 1.0);
        assert_eq!(
            host.evaluation_warnings()[0].kind,
            EvaluationWarningKind::ClampedNormalizedRisk
        );
    }

    #[test]
    fn evaluate_restores_guest_state_before_each_invocation() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module(
            r#"
    (local $counter i32)
    (local $memory_value i32)
    global.get $counter
    local.set $counter
    i32.const 512
    i32.load
    local.set $memory_value
    global.get $counter
    i32.const 1
    i32.add
    global.set $counter
    i32.const 512
    local.get $memory_value
    i32.const 1
    i32.add
    i32.store
    f32.const 0
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.const 32
    i64.shl
    local.get $counter
    local.get $memory_value
    i32.add
    f32.convert_i32_u
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.or
            "#,
            r#"  (global $counter (export "counter") (mut i32) (i32.const 41))"#,
            r#"
    i32.const 512
    i32.const 7
    i32.store
            "#,
            1,
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let first = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();
        let second = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(first.raw_value, 48.0);
        assert_eq!(second.raw_value, 48.0);
        assert_eq!(first, second);
    }

    #[test]
    fn evaluate_enforces_per_invocation_fuel_budget() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module(
            r#"
    (loop $spin
      br $spin
    )
    unreachable
            "#,
            "",
            "",
            1,
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        assert_eq!(
            host.evaluate(definition.as_ref(), &sample_request())
                .unwrap(),
            None
        );
        assert_eq!(
            host.evaluation_warnings()[0].kind,
            EvaluationWarningKind::PerInvocationFuelExhausted
        );
    }

    #[test]
    fn evaluate_skips_when_aggregate_budget_is_exhausted() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module("i64.const 0", "", "", 1);
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(workspace.path(), &manifest, &BTreeSet::new(), 0);
        let definition = host.load_metric_definitions().unwrap().remove(0);

        assert_eq!(
            host.evaluate(definition.as_ref(), &sample_request())
                .unwrap(),
            None
        );
        assert_eq!(
            host.evaluation_warnings()[0].kind,
            EvaluationWarningKind::AggregateFuelExhausted
        );
    }

    #[test]
    fn evaluate_enforces_linear_memory_limit() {
        let workspace = tempdir().unwrap();
        let wasm = build_evaluation_module(
            r#"
    (local $result i32)
    i32.const 1024
    memory.grow
    local.set $result
    f32.const 0
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.const 32
    i64.shl
    local.get $result
    f32.convert_i32_s
    i32.reinterpret_f32
    i64.extend_i32_u
    i64.or
            "#,
            "",
            "",
            1,
        );
        let checksum = write_plugin(workspace.path(), "plugin.wasm", &wasm);
        let manifest = manifest_entry("plugin.wasm", &checksum);
        let mut host = WasmPluginHost::load(
            workspace.path(),
            &manifest,
            &BTreeSet::new(),
            FULL_ANALYSIS_AGGREGATE_FUEL_BUDGET,
        );
        let definition = host.load_metric_definitions().unwrap().remove(0);

        let value = host
            .evaluate(definition.as_ref(), &sample_request())
            .unwrap()
            .unwrap();

        assert_eq!(value.raw_value, -1.0);
        assert_eq!(value.normalized_risk, 0.0);
    }

    #[derive(Clone, Copy)]
    struct RegistrationSpec<'a> {
        id: &'a str,
        level: u32,
        name: &'a str,
        description: &'a str,
    }

    fn sample_registration(id: &'static str) -> RegistrationSpec<'static> {
        RegistrationSpec {
            id,
            level: 1,
            name: "Plugin Metric",
            description: "Registered from wasm",
        }
    }

    fn manifest_entry(path: &str, checksum: &str) -> ResolvedPluginManifest {
        ResolvedPluginManifest {
            modules: vec![PluginModuleRef {
                workspace_relative_path: FilePath::from(path),
                sha256: checksum.to_owned(),
            }],
        }
    }

    fn write_plugin(workspace_root: &Path, name: &str, bytes: &[u8]) -> String {
        let path = workspace_root.join(name);
        fs::write(path, bytes).unwrap();
        sha256_hex(bytes)
    }

    fn sample_request() -> PluginEvaluationRequest {
        let location = SourceLocation {
            file_path: FilePath::from("src/lib.rs"),
            start_line: 1,
            end_line: 3,
        };
        PluginEvaluationRequest {
            scope_id: ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
            subgraph: CpgSubgraph {
                scope_id: ScopeId::new(AnalysisLevel::Module, "crate", "src/lib.rs"),
                nodes: vec![
                    CpgNode {
                        id: NodeId::from(1),
                        kind: NodeKind::Function,
                        name: "alpha".to_owned(),
                        location: location.clone(),
                        extension: None,
                    },
                    CpgNode {
                        id: NodeId::from(2),
                        kind: NodeKind::Module,
                        name: "beta".to_owned(),
                        location,
                        extension: None,
                    },
                ],
                edges: vec![CpgEdge {
                    source: NodeId::from(1),
                    target: NodeId::from(2),
                    kind: EdgeKind::Contains,
                    extension: None,
                }],
            },
            config: MetricConfig {
                entries: BTreeMap::from([("threshold".to_owned(), "0.875".to_owned())]),
            },
        }
    }

    fn build_test_module(
        spi_version: Option<&str>,
        init_return_code: i32,
        registrations: &[RegistrationSpec<'_>],
    ) -> Vec<u8> {
        let mut data_offset = 0_u32;
        let mut data_segments = String::new();
        let mut init_body = String::new();

        for registration in registrations {
            let id_offset = data_offset;
            data_segments.push_str(&format!(
                "  (data (i32.const {id_offset}) \"{}\")\n",
                wat_string(registration.id),
            ));
            data_offset += u32::try_from(registration.id.len()).unwrap() + 16;

            let name_offset = data_offset;
            data_segments.push_str(&format!(
                "  (data (i32.const {name_offset}) \"{}\")\n",
                wat_string(registration.name),
            ));
            data_offset += u32::try_from(registration.name.len()).unwrap() + 16;

            let description_offset = data_offset;
            data_segments.push_str(&format!(
                "  (data (i32.const {description_offset}) \"{}\")\n",
                wat_string(registration.description),
            ));
            data_offset += u32::try_from(registration.description.len()).unwrap() + 16;

            init_body.push_str(&format!(
                "    i32.const {id_offset}\n    i32.const {}\n    i32.const {}\n    i32.const {name_offset}\n    i32.const {}\n    i32.const {description_offset}\n    i32.const {}\n    call $metric_register\n    drop\n",
                registration.id.len(),
                registration.level,
                registration.name.len(),
                registration.description.len(),
            ));
        }

        let wat = format!(
            "(module
  (type $metric_register_t (func (param i32 i32 i32 i32 i32 i32 i32) (result i32)))
  (import \"kalos\" \"metric_register\" (func $metric_register (type $metric_register_t)))
  (memory (export \"memory\") 1)
{data_segments}  (func (export \"kalos_plugin_init\") (result i32)
{init_body}    i32.const {init_return_code}
  )
  (func (export \"kalos_plugin_evaluate\") (param i32 i32 i32 i32) (result i64)
    i64.const 0
  )
  (func (export \"kalos_plugin_alloc\") (param i32) (result i32)
    i32.const 0
  )
  (func (export \"kalos_plugin_free\") (param i32 i32))
)"
        );

        build_raw_test_module(spi_version, &wat)
    }

    fn build_evaluation_module(
        evaluate_body: &str,
        extra_items: &str,
        extra_init_body: &str,
        memory_pages: u32,
    ) -> Vec<u8> {
        let wat = format!(
            r#"(module
  (type $metric_register_t (func (param i32 i32 i32 i32 i32 i32 i32) (result i32)))
  (type $count_t (func (param i32 i32) (result i32)))
  (type $read_t (func (param i32 i32 i32 i32 i32) (result i32)))
  (type $config_t (func (param i32 i32 i32 i32) (result i32)))
  (import "kalos" "metric_register" (func $metric_register (type $metric_register_t)))
  (import "kalos" "cpg_node_count" (func $cpg_node_count (type $count_t)))
  (import "kalos" "cpg_edge_count" (func $cpg_edge_count (type $count_t)))
  (import "kalos" "cpg_read_node" (func $cpg_read_node (type $read_t)))
  (import "kalos" "cpg_read_edge" (func $cpg_read_edge (type $read_t)))
  (import "kalos" "config_read" (func $config_read (type $config_t)))
  (memory (export "memory") {memory_pages})
  (global $heap (export "heap") (mut i32) (i32.const 4096))
  (data (i32.const 32) "plugin.metric")
  (data (i32.const 96) "Plugin Metric")
  (data (i32.const 160) "Registered from wasm")
{extra_items}
  (func (export "kalos_plugin_init") (result i32)
    i32.const 32
    i32.const 13
    i32.const 1
    i32.const 96
    i32.const 13
    i32.const 160
    i32.const 18
    call $metric_register
    drop
{extra_init_body}
    i32.const 0
  )
  (func (export "kalos_plugin_evaluate") (param i32 i32 i32 i32) (result i64)
{evaluate_body}
  )
  (func (export "kalos_plugin_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $size
    i32.add
    global.set $heap
    memory.size
    i32.const 65536
    i32.mul
    global.get $heap
    i32.lt_u
    if
      global.get $heap
      i32.const 65535
      i32.add
      i32.const 65536
      i32.div_u
      memory.size
      i32.sub
      memory.grow
      drop
    end
    local.get $ptr
  )
  (func (export "kalos_plugin_free") (param i32 i32))
)"#
        );

        build_raw_test_module(Some(SPI_VERSION), &wat)
    }

    fn build_raw_test_module(spi_version: Option<&str>, wat_source: &str) -> Vec<u8> {
        let mut wasm = wat::parse_str(wat_source).unwrap();
        if let Some(version) = spi_version {
            append_custom_section(&mut wasm, "kalos_spi_version", version.as_bytes());
        }
        wasm
    }

    fn encode_metric_bits(raw_value: f32, normalized_risk: f32) -> i64 {
        let bits = u64::from(raw_value.to_bits()) | (u64::from(normalized_risk.to_bits()) << 32);
        bits as i64
    }

    fn append_custom_section(bytes: &mut Vec<u8>, name: &str, payload: &[u8]) {
        let mut section = Vec::new();
        encode_var_u32(u32::try_from(name.len()).unwrap(), &mut section);
        section.extend_from_slice(name.as_bytes());
        section.extend_from_slice(payload);

        bytes.push(0);
        encode_var_u32(u32::try_from(section.len()).unwrap(), bytes);
        bytes.extend_from_slice(&section);
    }

    fn encode_var_u32(mut value: u32, bytes: &mut Vec<u8>) {
        loop {
            let mut byte = u8::try_from(value & 0x7f).unwrap();
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn wat_string(value: &str) -> String {
        value
            .bytes()
            .map(|byte| match byte {
                b'"' => "\\22".to_owned(),
                b'\\' => "\\5c".to_owned(),
                0x20..=0x7e => char::from(byte).to_string(),
                _ => format!("\\{:02x}", byte),
            })
            .collect::<String>()
    }
}
