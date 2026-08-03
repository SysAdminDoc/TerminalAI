//! Runtime capability discovery for the installed agent binaries.
//!
//! Model catalogs and reasoning efforts are product data, not stable CLI
//! enums. This module asks each resolved binary what it supports, caches the
//! answer for the resolved path and version banner, and keeps the launcher
//! permissive when a probe cannot answer or a user enters a newer value.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

use crate::agent::{self, Agent, AgentBinary, ResolveError};
use crate::launch::Effort;

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_PROBE_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelCapability {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub supported_efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentCapabilities {
    pub agent: Agent,
    pub resolved_path: PathBuf,
    pub version: String,
    pub models: Vec<ModelCapability>,
    /// Ordered union of the effort values advertised by the runtime.
    pub efforts: Vec<String>,
    /// Protocol or feature names surfaced by the runtime. Unknown names are
    /// intentionally retained for forward compatibility.
    pub protocol_capabilities: Vec<String>,
    pub source: String,
    pub warning: Option<String>,
}

impl AgentCapabilities {
    pub fn efforts_for_model(&self, model: Option<&str>) -> Vec<String> {
        if let Some(model) = model {
            if let Some(found) = self.models.iter().find(|candidate| candidate.id == model) {
                if !found.supported_efforts.is_empty() {
                    return found.supported_efforts.clone();
                }
            }
        }
        self.efforts.clone()
    }

    /// Return diagnostics without rejecting the launch. A runtime may learn a
    /// new model or effort before this binary does, so these are warnings only.
    pub fn warnings_for(&self, model: Option<&str>, effort: Option<&Effort>) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(model) = model {
            if !self.models.is_empty() && !self.models.iter().any(|known| known.id == model) {
                warnings.push(format!(
                    "model {model:?} is not in the detected {} catalog; passing it through",
                    self.agent.label()
                ));
            }
        }
        if let Some(effort) = effort {
            let available = self.efforts_for_model(model);
            if !available.is_empty() && !available.iter().any(|known| known == effort.as_str()) {
                warnings.push(format!(
                    "reasoning effort {:?} is not advertised for {}; passing it through",
                    effort.as_str(),
                    model.unwrap_or("the selected model")
                ));
            } else if available.is_empty() && effort.is_custom() {
                warnings.push(format!(
                    "reasoning effort {:?} was not verified by the runtime probe; passing it through",
                    effort.as_str()
                ));
            }
        }
        warnings
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    version: String,
}

fn capability_cache() -> &'static Mutex<HashMap<CacheKey, AgentCapabilities>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, AgentCapabilities>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe the resolved executable once per path/version pair.
pub fn probe(
    agent: Agent,
    configured_path: Option<&Path>,
) -> Result<AgentCapabilities, CapabilityError> {
    let binary = agent::resolve(agent, configured_path)?;
    let (version, version_warning) = match agent::version_banner(&binary.path) {
        Ok(version) => (version.trim().to_owned(), None),
        Err(error) => ("unidentified".to_owned(), Some(error)),
    };
    let key = CacheKey {
        path: binary.path.clone(),
        version: version.clone(),
    };
    if let Ok(cache) = capability_cache().lock() {
        if let Some(cached) = cache.get(&key) {
            return Ok(cached.clone());
        }
    }

    let (mut capabilities, probe_warning) = match agent {
        Agent::Claude => probe_claude(&binary),
        Agent::Codex => probe_codex(&binary),
    };
    capabilities.agent = agent;
    capabilities.resolved_path = binary.path.clone();
    capabilities.version = version;
    append_warning(&mut capabilities.warning, version_warning);
    append_warning(&mut capabilities.warning, probe_warning);

    if let Ok(mut cache) = capability_cache().lock() {
        // A replacement at the same path must not leave an older version alive
        // as an equally valid cache entry.
        cache.retain(|candidate, _| candidate.path != binary.path || candidate == &key);
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, capabilities.clone());
    }
    Ok(capabilities)
}

#[derive(Debug, Default)]
struct ProbeData {
    models: Vec<ModelCapability>,
    efforts: Vec<String>,
    protocol_capabilities: Vec<String>,
    source: String,
}

fn probe_claude(binary: &AgentBinary) -> (AgentCapabilities, Option<String>) {
    let base = AgentCapabilities {
        agent: binary.agent,
        resolved_path: binary.path.clone(),
        version: String::new(),
        models: Vec::new(),
        efforts: Vec::new(),
        protocol_capabilities: Vec::new(),
        source: "claude stream-json system/init".into(),
        warning: None,
    };
    let result = (|| -> Result<ProbeData, String> {
        let mut process = ProbeProcess::spawn(
            &binary.path,
            [
                "--bare",
                "--no-session-persistence",
                "--print",
                "capability probe",
                "--output-format",
                "stream-json",
                "--verbose",
            ],
        )?;
        loop {
            let line = process.next_line(PROBE_TIMEOUT)?;
            let value: Value = serde_json::from_str(&line)
                .map_err(|error| format!("Claude startup event was not JSON: {error}"))?;
            if value.get("type").and_then(Value::as_str) == Some("system")
                && value.get("subtype").and_then(Value::as_str) == Some("init")
            {
                break parse_claude_init(&value);
            }
        }
    })();
    match result {
        Ok(mut data) => {
            data.source = "claude stream-json system/init".into();
            (
                AgentCapabilities {
                    models: data.models,
                    efforts: data.efforts,
                    protocol_capabilities: data.protocol_capabilities,
                    source: data.source,
                    ..base
                },
                None,
            )
        }
        Err(error) => (
            base,
            Some(format!(
                "Claude capability probe unavailable: {error}; free-text values remain allowed"
            )),
        ),
    }
}

fn probe_codex(binary: &AgentBinary) -> (AgentCapabilities, Option<String>) {
    let base = AgentCapabilities {
        agent: binary.agent,
        resolved_path: binary.path.clone(),
        version: String::new(),
        models: Vec::new(),
        efforts: Vec::new(),
        protocol_capabilities: Vec::new(),
        source: "codex app-server model/list".into(),
        warning: None,
    };
    let mut warnings = Vec::new();
    let mut data = match probe_codex_app_server(&binary.path) {
        Ok(data) => data,
        Err(error) => {
            warnings.push(format!(
                "Codex model/list unavailable ({error}); trying codex debug models"
            ));
            match capture_command(&binary.path, ["debug", "models"]) {
                Ok(output) => match serde_json::from_str::<Value>(&output) {
                    Ok(value) => match parse_codex_model_catalog(&value) {
                        Ok(mut data) => {
                            data.source = "codex debug models + features".into();
                            data
                        }
                        Err(error) => {
                            warnings.push(error);
                            ProbeData {
                                source: "codex features list".into(),
                                ..ProbeData::default()
                            }
                        }
                    },
                    Err(error) => {
                        warnings.push(format!("codex debug models was not JSON: {error}"));
                        ProbeData {
                            source: "codex features list".into(),
                            ..ProbeData::default()
                        }
                    }
                },
                Err(error) => {
                    warnings.push(format!("Codex model catalog unavailable: {error}"));
                    ProbeData {
                        source: "codex features list".into(),
                        ..ProbeData::default()
                    }
                }
            }
        }
    };

    match capture_command(&binary.path, ["features", "list"]) {
        Ok(output) => data
            .protocol_capabilities
            .extend(parse_codex_features(&output)),
        Err(error) => warnings.push(format!("Codex feature probe unavailable: {error}")),
    }
    unique_strings(&mut data.efforts);
    unique_strings(&mut data.protocol_capabilities);
    (
        AgentCapabilities {
            models: data.models,
            efforts: data.efforts,
            protocol_capabilities: data.protocol_capabilities,
            source: data.source,
            ..base
        },
        (!warnings.is_empty()).then(|| warnings.join("; ")),
    )
}

fn probe_codex_app_server(path: &Path) -> Result<ProbeData, String> {
    let mut process = ProbeProcess::spawn(path, ["app-server", "--listen", "stdio://"])?;
    process.send_json(&json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "terminalai-capability-probe",
                "title": "TerminalAI capability probe",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": { "experimentalApi": true }
        }
    }))?;
    let initialize = wait_for_response(&mut process, 1)?;
    if initialize.get("error").is_some() {
        return Err(format!("initialize returned {}", initialize["error"]));
    }
    process.send_json(&json!({ "method": "initialized", "params": {} }))?;
    let mut request_id = 2;
    let mut cursor: Option<String> = None;
    let mut data = ProbeData {
        source: "codex app-server model/list + features".into(),
        ..ProbeData::default()
    };
    loop {
        let mut params = serde_json::Map::new();
        params.insert("includeHidden".into(), Value::Bool(false));
        if let Some(cursor) = cursor.as_deref() {
            params.insert("cursor".into(), Value::String(cursor.to_owned()));
        }
        process.send_json(&json!({
            "id": request_id,
            "method": "model/list",
            "params": params
        }))?;
        let models = wait_for_response(&mut process, request_id)?;
        if let Some(error) = models.get("error") {
            return Err(format!("model/list returned {error}"));
        }
        let page = parse_codex_model_catalog(models.get("result").unwrap_or(&Value::Null))?;
        merge_probe_data(&mut data, page);
        cursor = models["result"]
            .get("nextCursor")
            .or_else(|| models["result"].get("next_cursor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        request_id = request_id.saturating_add(1);
    }
    Ok(data)
}

fn wait_for_response(process: &mut ProbeProcess, id: u64) -> Result<Value, String> {
    loop {
        let line = process.next_line(PROBE_TIMEOUT)?;
        let value: Value = serde_json::from_str(&line)
            .map_err(|error| format!("Codex app-server emitted invalid JSON: {error}"))?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(value);
        }
    }
}

fn parse_claude_init(value: &Value) -> Result<ProbeData, String> {
    if value.get("type").and_then(Value::as_str) != Some("system")
        || value.get("subtype").and_then(Value::as_str) != Some("init")
    {
        return Err("first stream event was not system/init".into());
    }
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut efforts = string_values(value, &["supportedReasoningEfforts", "supported_efforts"]);
    let mut protocol_capabilities = string_values(value, &["capabilities"]);
    efforts.extend(string_values(value, &["efforts"]));
    unique_strings(&mut efforts);
    unique_strings(&mut protocol_capabilities);
    let models = model
        .map(|id| {
            vec![ModelCapability {
                id,
                display_name: None,
                supported_efforts: efforts.clone(),
                default_effort: None,
                hidden: false,
            }]
        })
        .unwrap_or_default();
    Ok(ProbeData {
        models,
        efforts,
        protocol_capabilities,
        source: "claude stream-json system/init".into(),
    })
}

fn parse_codex_model_catalog(value: &Value) -> Result<ProbeData, String> {
    let data = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| "model catalog did not contain a data/models array".to_owned())?;
    let mut models = Vec::new();
    for item in data {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(id) = ["id", "model", "slug"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let mut supported_efforts = string_values(
            item,
            &["supportedReasoningEfforts", "supported_reasoning_levels"],
        );
        unique_strings(&mut supported_efforts);
        models.push(ModelCapability {
            id: id.to_owned(),
            display_name: ["displayName", "display_name"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str).map(str::to_owned)),
            default_effort: ["defaultReasoningEffort", "default_reasoning_level"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str).map(str::to_owned)),
            hidden: object
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            supported_efforts,
        });
    }
    let mut efforts = Vec::new();
    for model in &models {
        efforts.extend(model.supported_efforts.iter().cloned());
    }
    unique_strings(&mut efforts);
    Ok(ProbeData {
        models,
        efforts,
        source: "codex model catalog".into(),
        ..ProbeData::default()
    })
}

fn parse_codex_features(output: &str) -> Vec<String> {
    let mut features = Vec::new();
    for line in output.lines() {
        let Some(name) = line.split_ascii_whitespace().next() else {
            continue;
        };
        if name.eq_ignore_ascii_case("feature") || name.starts_with('-') {
            continue;
        }
        if name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        {
            features.push(format!("codex.feature.{name}"));
        }
    }
    unique_strings(&mut features);
    features
}

fn string_values(value: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| {
                            value
                                .get("reasoningEffort")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .or_else(|| {
                            value
                                .get("reasoning_effort")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .or_else(|| {
                            value
                                .get("effort")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn unique_strings(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| !value.is_empty() && seen.insert(value.clone()));
}

fn merge_probe_data(target: &mut ProbeData, mut page: ProbeData) {
    target.models.append(&mut page.models);
    target.efforts.append(&mut page.efforts);
    target
        .protocol_capabilities
        .append(&mut page.protocol_capabilities);
    unique_model_capabilities(&mut target.models);
    unique_strings(&mut target.efforts);
    unique_strings(&mut target.protocol_capabilities);
}

fn unique_model_capabilities(models: &mut Vec<ModelCapability>) {
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.id.clone()));
}

fn append_warning(target: &mut Option<String>, warning: Option<String>) {
    let Some(warning) = warning else {
        return;
    };
    match target {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&warning);
        }
        None => *target = Some(warning),
    }
}

struct ProbeProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<io::Result<String>>,
    reader: Option<JoinHandle<()>>,
}

impl ProbeProcess {
    fn spawn<I, S>(path: &Path, args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(path);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start capability probe: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "capability probe did not provide stdout".to_owned())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "capability probe did not provide stdin".to_owned())?;
        let (sender, lines) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("terminalai-capability-reader".into())
            .spawn(move || read_lines(stdout, sender))
            .map_err(|error| format!("could not start capability reader: {error}"))?;
        Ok(Self {
            child,
            stdin,
            lines,
            reader: Some(reader),
        })
    }

    fn send_json(&mut self, value: &Value) -> Result<(), String> {
        let line = serde_json::to_string(value).map_err(|error| error.to_string())?;
        writeln!(self.stdin, "{line}")
            .map_err(|error| format!("capability probe write failed: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("capability probe flush failed: {error}"))
    }

    fn next_line(&mut self, timeout: Duration) -> Result<String, String> {
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(error)) => Err(format!("capability probe read failed: {error}")),
            Err(RecvTimeoutError::Timeout) => Err("capability probe timed out".into()),
            Err(RecvTimeoutError::Disconnected) => {
                Err("capability probe exited before reporting capabilities".into())
            }
        }
    }
}

impl Drop for ProbeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_lines(stdout: impl Read, sender: mpsc::Sender<io::Result<String>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(bytes) if bytes > MAX_PROBE_LINE_BYTES => {
                let _ = sender.send(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "capability probe line is too large",
                )));
                break;
            }
            Ok(_) => {
                if sender.send(Ok(line)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    }
}

fn capture_command<I, S>(path: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start capability command: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "capability command did not provide stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "capability command did not provide stderr".to_owned())?;
    let stdout_reader = spawn_capture_reader(stdout)?;
    let stderr_reader = spawn_capture_reader(stderr)?;
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("capability command timed out".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for capability command: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .unwrap_or_else(|_| Err("stdout reader panicked".into()))?;
    let stderr = stderr_reader
        .join()
        .unwrap_or_else(|_| Err("stderr reader panicked".into()))?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        return Err(format!("command exited with {status}: {}", detail.trim()));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

fn spawn_capture_reader(
    mut reader: impl Read + Send + 'static,
) -> Result<JoinHandle<Result<Vec<u8>, String>>, String> {
    thread::Builder::new()
        .name("terminalai-capability-capture".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let read = reader
                    .read(&mut chunk)
                    .map_err(|error| format!("capability output read failed: {error}"))?;
                if read == 0 {
                    return Ok(bytes);
                }
                if bytes.len().saturating_add(read) > MAX_CAPTURE_BYTES {
                    return Err("capability command output is too large".into());
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
        })
        .map_err(|error| format!("could not start capability capture: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_model_catalog_keeps_per_model_effort_order() {
        let value = serde_json::json!({
            "data": [
                {"id": "alpha", "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "ultra"}]},
                {"model": "beta", "supported_reasoning_levels": [{"effort": "medium"}]}
            ]
        });
        let data = parse_codex_model_catalog(&value).expect("catalog");
        assert_eq!(data.models[0].id, "alpha");
        assert_eq!(data.models[0].supported_efforts, ["low", "ultra"]);
        assert_eq!(data.models[1].supported_efforts, ["medium"]);
        assert_eq!(data.efforts, ["low", "ultra", "medium"]);
    }

    #[test]
    fn claude_init_retains_unknown_protocol_capabilities_and_model() {
        let value = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "model": "claude-fable-5",
            "capabilities": ["interrupt_receipt_v1", "future_protocol_v2"]
        });
        let data = parse_claude_init(&value).expect("system/init");
        assert_eq!(data.models[0].id, "claude-fable-5");
        assert_eq!(
            data.protocol_capabilities,
            ["interrupt_receipt_v1", "future_protocol_v2"]
        );
    }

    #[test]
    fn runtime_feature_output_is_not_a_compile_time_allowlist() {
        assert_eq!(
            parse_codex_features("feature status\nfuture_reasoning stable\nmodel_catalog stable\n"),
            [
                "codex.feature.future_reasoning",
                "codex.feature.model_catalog"
            ]
        );
    }

    #[test]
    fn warnings_keep_unknown_values_launchable() {
        let capabilities = AgentCapabilities {
            agent: Agent::Codex,
            resolved_path: PathBuf::from("codex.exe"),
            version: "codex-cli 0.146.0".into(),
            models: vec![ModelCapability {
                id: "alpha".into(),
                display_name: None,
                supported_efforts: vec!["low".into()],
                default_effort: None,
                hidden: false,
            }],
            efforts: vec!["low".into()],
            protocol_capabilities: Vec::new(),
            source: "test".into(),
            warning: None,
        };
        let warnings =
            capabilities.warnings_for(Some("new-model"), Some(&Effort::Custom("ultra".into())));
        assert_eq!(warnings.len(), 2);
        assert!(warnings
            .iter()
            .all(|warning| warning.contains("passing it through")));
    }
}
