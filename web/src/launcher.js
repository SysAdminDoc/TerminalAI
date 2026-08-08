/**
 * The launch dialog: configuration, capability discovery, project templates,
 * registered roots, presets, and the final launch action.
 *
 * Split out of `main.js` as one cohesive surface. The factory keeps the live
 * application state and shell helpers owned by the entry module while making
 * the launcher's source independently auditable and testable.
 */
export function createLauncher(deps) {
  const {
    $,
    document: doc,
    state,
    invoke,
    invokeArgs,
    showToast,
    t,
    escapeHtml,
    renderDataError,
    renderProjects,
  } = deps;

  function defaultSpec() {
    return {
      agent: "claude",
      name: null,
      cwd: "",
      model: null,
      effort: null,
      permission: "ask",
      sandbox: null,
      profile: null,
      add_dirs: [],
      resume: { kind: "new" },
      max_budget_usd: null,
      max_concurrent_subagents: null,
      agent_teams: null,
      web_search: false,
      initial_prompt: null,
      extra_args: [],
      allowed_tools: [],
      disallowed_tools: [],
      settings: null,
      setting_sources: null,
      mcp_config: [],
      strict_mcp_config: false,
      plugin_dirs: [],
      plugin_urls: [],
      fallback_model: null,
      environment: { setup: null, teardown: null, port_base: 42000, port_count: 4 },
      worktree: false,
    };
  }

  /// A whole-number field, or null when it is blank.
  ///
  /// Blank and zero are different requests: blank means the agent's own default,
  /// and zero is refused by the core rather than read as "no cap".
  function optionalCount(id) {
    const raw = $(id).value.trim();
    if (!raw) return null;
    const value = Number(raw);
    return Number.isInteger(value) ? value : null;
  }

  /// The three-state teams choice. `null` leaves it to the agent's own
  /// configuration; the other two state it, because "teams off" is a decision
  /// about a session's cost and should not depend on ambient configuration.
  function teamsChoice(value) {
    if (value === "on") return true;
    if (value === "off") return false;
    return null;
  }

  /// One comma-separated field as a list, with empty entries dropped. An empty
  /// entry would reach the agent as a bare flag with nothing after it.
  function commaList(id) {
    return $(id)
      .value.split(",")
      .map((entry) => entry.trim())
      .filter(Boolean);
  }

  function readSpec() {
    const agent = $("agent-input").value;
    const resumeKind = $("resume-input").value;
    const nativeId = $("resume-id-input").value.trim();
    const resume =
      resumeKind === "session"
        ? { kind: "session", id: nativeId }
        : resumeKind === "fork"
          ? { kind: "fork", id: nativeId }
          : { kind: resumeKind };
    const budget = $("budget-input").value.trim();
    const portBase = Number.parseInt($("port-base-input").value, 10);
    const portCount = Number.parseInt($("port-count-input").value, 10);
    return {
      agent,
      name: $("name-input").value.trim() || null,
      cwd: $("cwd-input").value.trim(),
      model: $("model-input").value.trim() || null,
      effort: $("effort-input").value.trim() || null,
      permission: $("permission-input").value,
      sandbox: agent === "codex" ? $("sandbox-input").value : null,
      profile: agent === "codex" ? $("profile-input").value.trim() || null : null,
      add_dirs: [...state.extraDirs],
      resume,
      // Both agents: the cap is enforced by this tool's ledger, which reads
      // both agents' transcripts, rather than by a launcher flag only one of
      // them has and neither honours outside print mode.
      max_budget_usd: budget ? Number(budget) : null,
      web_search: agent === "codex" && $("search-input").checked,
      // Claude-only, and sent only for Claude so switching agents does not carry
      // a field the core would refuse. Admission governs how many sessions run;
      // this is the one multiplier a single session controls.
      max_concurrent_subagents: agent === "claude" ? optionalCount("subagents-input") : null,
      agent_teams: agent === "claude" ? teamsChoice($("agent-teams-input").value) : null,
      initial_prompt: $("prompt-input").value.trim() || null,
      extra_args: [],
      worktree: $("worktree-input").checked,
      agent_home: $("agent-home-input").value.trim() || null,
      // Names only. The core reads each value from this process and refuses a
      // name that is unset, so an empty entry must not reach it as one.
      env_passthrough: $("env-passthrough-input")
        .value.split(",")
        .map((name) => name.trim())
        .filter(Boolean),
      // Claude-only on the versions this build maps. Sent only for Claude so a
      // Codex launch is not refused for a field the operator left behind when
      // switching agents — the core refuses these on Codex by design, and that
      // refusal should describe a choice, not a stale form.
      allowed_tools: agent === "claude" ? commaList("allowed-tools-input") : [],
      disallowed_tools: agent === "claude" ? commaList("disallowed-tools-input") : [],
      settings: agent === "claude" ? $("settings-input").value.trim() || null : null,
      setting_sources: agent === "claude" ? $("setting-sources-input").value.trim() || null : null,
      mcp_config: agent === "claude" ? commaList("mcp-config-input") : [],
      strict_mcp_config: agent === "claude" && $("strict-mcp-input").checked,
      plugin_dirs: agent === "claude" ? commaList("plugin-dirs-input") : [],
      plugin_urls: agent === "claude" ? commaList("plugin-urls-input") : [],
      // Deliberately never set. `claude --help` restricts `--fallback-model` to
      // `--print`, so the launcher offered a control the agent would ignore;
      // the core refuses the field by name, and a stored preset that still
      // carries one is refused rather than launched with a flag that does
      // nothing.
      fallback_model: null,
      environment: {
        setup: $("setup-hook-input").value.trim() || null,
        teardown: $("teardown-hook-input").value.trim() || null,
        port_base: Number.isInteger(portBase) ? portBase : 42000,
        port_count: Number.isInteger(portCount) ? portCount : 4,
      },
    };
  }

  // A <select> silently discards a value it has no option for, so a preset or a
  // resumed spec naming a permission mode this build does not model would come
  // back as "" and launch with no mode at all. The core keeps such a value
  // (Permission::Custom); this carries it into the list so it round-trips and
  // the operator can see what is about to be launched. Previous carried-in
  // options are dropped first so switching presets does not accumulate them.
  function setPermissionValue(value) {
    const select = $("permission-input");
    for (const option of Array.from(select.options)) {
      if (option.dataset.passthrough === "true") option.remove();
    }
    const wanted = value ?? "ask";
    if (!Array.from(select.options).some((option) => option.value === wanted)) {
      const option = doc.createElement("option");
      option.value = wanted;
      option.textContent = t("launcher-permission-custom", { mode: wanted });
      option.dataset.passthrough = "true";
      select.append(option);
    }
    select.value = wanted;
  }

  function writeSpec(spec) {
    clearFolderValidation();
    $("agent-input").value = spec.agent ?? "claude";
    $("name-input").value = spec.name ?? "";
    // A built-in preset names no folder: which configuration and which project
    // are separate choices, so applying "Plan first" must not retarget the
    // session to nowhere. Only a preset that actually carries a folder sets one.
    if (spec.cwd) $("cwd-input").value = spec.cwd;
    $("model-input").value = spec.model ?? "";
    $("effort-input").value = spec.effort ?? "";
    setPermissionValue(spec.permission);
    $("sandbox-input").value = spec.sandbox ?? "workspace-write";
    $("profile-input").value = spec.profile ?? "";
    $("resume-input").value = spec.resume?.kind ?? "new";
    $("resume-id-input").value = spec.resume?.id ?? "";
    $("budget-input").value = spec.max_budget_usd ?? "";
    $("search-input").checked = Boolean(spec.web_search);
    $("subagents-input").value = spec.max_concurrent_subagents ?? "";
    $("agent-teams-input").value = spec.agent_teams === true ? "on" : spec.agent_teams === false ? "off" : "";
    $("worktree-input").checked = Boolean(spec.worktree);
    $("agent-home-input").value = spec.agent_home ?? "";
    $("env-passthrough-input").value = (spec.env_passthrough ?? []).join(", ");
    $("allowed-tools-input").value = (spec.allowed_tools ?? []).join(", ");
    $("disallowed-tools-input").value = (spec.disallowed_tools ?? []).join(", ");
    $("settings-input").value = spec.settings ?? "";
    $("setting-sources-input").value = spec.setting_sources ?? "";
    $("mcp-config-input").value = (spec.mcp_config ?? []).join(", ");
    $("strict-mcp-input").checked = Boolean(spec.strict_mcp_config);
    $("plugin-dirs-input").value = (spec.plugin_dirs ?? []).join(", ");
    $("plugin-urls-input").value = (spec.plugin_urls ?? []).join(", ");
    $("port-base-input").value = spec.environment?.port_base ?? 42000;
    $("port-count-input").value = spec.environment?.port_count ?? 4;
    $("setup-hook-input").value = spec.environment?.setup ?? "";
    $("teardown-hook-input").value = spec.environment?.teardown ?? "";
    $("prompt-input").value = spec.initial_prompt ?? "";
    state.extraDirs = spec.add_dirs ?? [];
    $("extra-dirs-input").value = state.extraDirs.join("; ");
    syncAgentFields();
    schedulePreview();
  }

  function clearFolderValidation() {
    const input = $("cwd-input");
    input.removeAttribute("aria-invalid");
    input.setCustomValidity("");
    const message = $("cwd-error");
    message.hidden = true;
    message.textContent = "";
  }

  function showFolderValidation() {
    const input = $("cwd-input");
    const message = $("cwd-error");
    const text = t("launcher-folder-required");
    input.setAttribute("aria-invalid", "true");
    input.setCustomValidity(text);
    message.textContent = text;
    message.hidden = false;
    input.focus();
  }

  /*
function capabilityForAgent(agent = $("agent-input").value) {
  */
  function capabilityForAgent(agent = $("agent-input").value) {
    return state.capabilities[agent] ?? null;
  }

  function renderCapabilityFields() {
    const capabilities = capabilityForAgent();
    const selectedModel = $("model-input").value.trim();
    const models = Array.isArray(capabilities?.models) ? capabilities.models : [];
    $("model-suggestions").innerHTML = models
      .filter((model) => !model.hidden)
      .map((model) => `<option value="${escapeHtml(model.id)}"></option>`)
      .join("");
    const selected = models.find((model) => model.id === selectedModel);
    const efforts = selected?.supported_efforts?.length ? selected.supported_efforts : (capabilities?.efforts ?? []);
    $("effort-suggestions").innerHTML = efforts
      .map((effort) => `<option value="${escapeHtml(effort)}"></option>`)
      .join("");
    const warnings = [];
    if (capabilities?.warning) warnings.push(capabilities.warning);
    if (selectedModel && models.length && !models.some((model) => model.id === selectedModel)) {
      warnings.push(`Model ${selectedModel} is not in the detected catalog; it will be passed through.`);
    }
    const selectedEffort = $("effort-input").value.trim();
    if (selectedEffort && efforts.length && !efforts.includes(selectedEffort)) {
      warnings.push(`Reasoning effort ${selectedEffort} is not advertised for this model; it will be passed through.`);
    }
    const note = $("capability-note");
    note.classList.toggle("field-hidden", warnings.length === 0);
    note.textContent = warnings.join(" ");
  }

  async function loadAgentCapabilities(agent = $("agent-input").value) {
    const request = ++state.capabilityRequest;
    renderCapabilityFields();
    try {
      const capabilities = await invoke("agent_capabilities", { agent, configuredPath: null });
      if (request !== state.capabilityRequest) return;
      state.capabilities[agent] = capabilities;
    } catch (error) {
      if (request !== state.capabilityRequest) return;
      state.capabilities[agent] = {
        models: [],
        efforts: [],
        warning: `Runtime capability probe unavailable: ${String(error)} Custom values remain allowed.`,
      };
    }
    renderCapabilityFields();
  }

  function syncAgentFields() {
    const codex = $("agent-input").value === "codex";
    doc.querySelectorAll(".codex-only").forEach((element) => element.classList.toggle("field-hidden", !codex));
    doc.querySelectorAll(".claude-only").forEach((element) => element.classList.toggle("field-hidden", codex));
    renderCapabilityFields();
    // Choosing Claude used to silently rewrite a plan-mode selection to "ask".
    // It had been there unchanged since the first Tauri shell commit, with no
    // test and no recorded reason, and it rewrote two of this tool's own built-in
    // presets the moment the launcher synced its fields.
    //
    // Removed 2026-08-07 after verifying against the installed build rather than
    // the documentation: `claude --help` lists `plan` among the accepted
    // `--permission-mode` choices, and `claude --permission-mode plan --print`
    // runs and exits 0. `launch.rs` has always mapped Permission::Plan for both
    // agents, so the launcher was the only thing that disagreed.
    doc.querySelectorAll(".resume-id-field").forEach((element) =>
      element.classList.toggle(
        "field-hidden",
        $("resume-input").value === "new" || $("resume-input").value === "last",
      ),
    );
  }
  /*
}
*/

  function schedulePreview() {
    clearTimeout(state.previewTimer);
    const request = ++state.previewRequest;
    state.previewTimer = setTimeout(() => updatePreview(request), 180);
  }

  async function updatePreview(request) {
    const spec = readSpec();
    if (!spec.cwd) {
      $("preview-output").textContent = t("preview-folder");
      $("preview-state").textContent = t("preview-waiting");
      return;
    }
    $("preview-state").textContent = t("preview-resolving");
    try {
      const command = await invoke("preview_launch", invokeArgs(spec));
      if (request !== state.previewRequest) return;
      $("preview-output").textContent = command;
      $("preview-state").textContent = t("preview-exact");
    } catch (error) {
      if (request !== state.previewRequest) return;
      $("preview-output").textContent = String(error);
      $("preview-state").textContent = t("preview-refused");
    }
  }

  async function launchCurrentSpec() {
    const spec = readSpec();
    if (!spec.cwd) {
      showFolderValidation();
      return false;
    }
    clearFolderValidation();
    try {
      const receipt = await invoke("launch_session", invokeArgs(spec));
      $("launcher-dialog").close();
      const agentLabel = spec.agent === "codex" ? "Codex" : "Claude Code";
      showToast(
        receipt?.queued
          ? agentLabel + " session queued for an admission slot"
          : agentLabel + " session launched",
        "success",
      );
      return true;
    } catch (error) {
      showToast(String(error));
      return false;
    }
  }

  async function loadPresets() {
    try {
      state.presets = await invoke("list_presets");
      const selected = $("preset-select").value;
      // Built-ins are labelled, not silently mixed in: an operator who cannot
      // see which ones shipped with the app cannot tell why one of them refuses
      // to be overwritten.
      $("preset-select").innerHTML = `<option value="">${escapeHtml(t("button-presets"))}</option>${state.presets
        .map((preset) => {
          const label = preset.builtin ? `${preset.name} ${t("preset-builtin-mark")}` : preset.name;
          const title = preset.description ? ` — ${preset.description}` : "";
          return (
            `<option value="${escapeHtml(preset.name)}" title="${escapeHtml(`${label}${title}`)}">` +
            `${escapeHtml(label)}</option>`
          );
        })
        .join("")}`;
      if (state.presets.some((preset) => preset.name === selected)) $("preset-select").value = selected;
      $("delete-preset-button").disabled = !$("preset-select").value;
    } catch (error) {
      showToast(t("presets-load-error", { error: String(error) }));
    }
  }

  /**
   * Offer the launch configurations the chosen repository declares about itself.
   *
   * Re-read every time the folder changes rather than cached: the file is
   * versioned with the repository, so pulling a branch that changes it should
   * change what the launcher offers.
   *
   * A repository with no templates hides the control entirely — an empty dropdown
   * reads as "this project has none configured yet", which is a different and
   * more distracting claim than not mentioning it.
   */
  async function loadProjectTemplates() {
    const field = doc.querySelector(".project-template-field");
    const cwd = $("cwd-input").value.trim();
    state.templates = [];
    if (!cwd) {
      field.hidden = true;
      return;
    }
    try {
      state.templates = await invoke("list_templates", { cwd });
    } catch (error) {
      // Said out loud, never swallowed: launching now would apply the operator's
      // own defaults while they believe the project's were used.
      field.hidden = true;
      showToast(t("template-unreadable", { detail: String(error) }));
      return;
    }
    field.hidden = state.templates.length === 0;
    $("template-select").innerHTML = `<option value="">${escapeHtml(t("template-none"))}</option>${state.templates
      .map(
        (template, index) =>
          `<option value="${index}">${escapeHtml(template.name)}${
            template.description ? ` — ${escapeHtml(template.description)}` : ""
          }</option>`,
      )
      .join("")}`;
  }

  /**
   * Apply the chosen template to the form.
   *
   * The folder is deliberately not touched: it is the repository the template
   * was read from, which is the one choice the operator has already made.
   */
  /*
function applyProjectTemplate() {
  */
  function applyProjectTemplate() {
    const index = Number.parseInt($("template-select").value, 10);
    const template = state.templates[index];
    if (!template) return;
    const cwd = $("cwd-input").value.trim();
    if (template.agent) $("agent-input").value = template.agent;
    if (template.model) $("model-input").value = template.model;
    if (template.effort) $("effort-input").value = template.effort;
    if (template.permission) setPermissionValue(template.permission);
    if (template.sandbox) $("sandbox-input").value = template.sandbox;
    if (template.profile) $("profile-input").value = template.profile;
    if (template.prompt) $("prompt-input").value = template.prompt;
    $("worktree-input").checked = Boolean(template.worktree);
    $("search-input").checked = Boolean(template.web_search);
    state.extraDirs = (template.add_dirs ?? []).map((dir) => `${cwd}/${dir}`);
    $("extra-dirs-input").value = state.extraDirs.join("; ");
    syncAgentFields();
    schedulePreview();
    showToast(t("template-applied", { name: template.name }), "success");
  }

  /**
   * Offer every repository under the registered roots as a launch target.
   *
   * Re-read rather than cached: the point of the list is being current, so a
   * repository cloned five minutes ago is launchable without telling the app.
   *
   * With no root registered the control is hidden entirely — an empty "Known
   * projects" dropdown is a question the operator has no way to answer. The
   * register button beside Browse is what they see instead.
   */
  function renderProjectRoots() {
    const list = $("project-root-list");
    if (state.projectRootsError) {
      renderDataError(
        list,
        t("projects-roots-load-error", { error: state.projectRootsError }),
        "project-roots",
        loadProjectRoots,
      );
      return;
    }
    if (!state.projectRoots.length) {
      list.innerHTML = '<li class="rollup-total">' + escapeHtml(t("projects-roots-empty")) + "</li>";
      return;
    }
    list.innerHTML = state.projectRoots
      .map((root) => {
        const value = String(root);
        const label = t("projects-root-remove", { root: value });
        return (
          '<li class="project-root-row"><code class="project-root-path" title="' +
          escapeHtml(value) +
          '">' +
          escapeHtml(value) +
          '</code><button type="button" class="button button-quiet" data-project-root-remove="' +
          escapeHtml(value) +
          '" title="' +
          escapeHtml(label) +
          '" aria-label="' +
          escapeHtml(label) +
          '">' +
          escapeHtml(t("button-remove")) +
          "</button></li>"
        );
      })
      .join("");
  }

  async function loadProjectRoots() {
    try {
      state.projectRoots = await invoke("list_project_roots");
      state.projectRootsError = null;
    } catch (error) {
      state.projectRoots = [];
      state.projectRootsError = String(error);
    }
    renderProjectRoots();
  }

  async function refreshScannedProjects() {
    try {
      state.scannedProjects = await invoke("scan_projects");
      state.projectsError = null;
    } catch (error) {
      state.scannedProjects = [];
      state.projectsError = String(error);
    }
    renderProjects();
  }

  async function removeProjectRoot(path) {
    try {
      const removed = await invoke("remove_project_root", { path });
      await loadProjectRoots();
      await loadKnownProjects();
      if ($("projects-dialog").open) await refreshScannedProjects();
      showToast(
        removed ? t("projects-root-removed", { root: path }) : t("projects-root-not-found", { root: path }),
        removed ? "success" : "",
      );
    } catch (error) {
      showToast(String(error));
    }
  }

  /*
async function loadKnownProjects() {
  */
  async function loadKnownProjects() {
    const field = doc.querySelector(".known-projects-field");
    try {
      state.projects = await invoke("list_projects");
    } catch (error) {
      state.projects = [];
      showToast(String(error));
    }
    field.hidden = state.projects.length === 0;
    $("register-root-empty-button").hidden = state.projects.length > 0;
    $("project-select").innerHTML = `<option value="">${escapeHtml(t("project-choose"))}</option>${state.projects
      .map(
        (project) =>
          `<option value="${escapeHtml(project.path)}" title="${escapeHtml(project.path)}">` +
            `${escapeHtml(project.name)}</option>`,
      )
      .join("")}`;
  }

  /**
   * Register a folder that holds repositories.
   *
   * Reports how many projects it found. "Registered" alone leaves the operator
   * unable to tell a working root from one pointed at the wrong directory, and
   * the difference only shows up later as an empty dropdown.
   */
  /*
async function registerProjectRoot() {
  */
  async function registerProjectRoot() {
    let root;
    try {
      root = await invoke("pick_folder");
    } catch (error) {
      showToast(String(error));
      return;
    }
    if (!root) return;
    try {
      await invoke("add_project_root", { path: root });
    } catch (error) {
      showToast(String(error));
      return;
    }
    await Promise.all([loadProjectRoots(), loadKnownProjects()]);
    if ($("projects-dialog").open) await refreshScannedProjects();
    const found = state.projects.filter((project) => project.root === root).length;
    showToast(
      found ? t("projects-root-added", { root, count: found }) : t("projects-none-found", { root }),
      found ? "success" : "",
    );
  }

  /*
async function saveCurrentPreset() {
  */
  async function saveCurrentPreset() {
    const name = $("preset-name-input").value.trim();
    if (!name) {
      showToast(t("preset-name-required"));
      $("preset-name-input").focus();
      return;
    }
    try {
      await invoke("save_preset", {
        preset: { name, spec: readSpec(), configured_path: null, builtin: false, description: null },
      });
      await loadPresets();
      $("preset-name-input").value = "";
      showToast(t("preset-saved", { name }), "success");
    } catch (error) {
      showToast(String(error));
    }
  }

  async function deleteSelectedPreset() {
    const select = $("preset-select");
    const name = select.value;
    if (!name) return;
    try {
      const removed = await invoke("delete_preset", { name });
      await loadPresets();
      showToast(
        removed ? t("preset-deleted", { name }) : t("preset-not-found", { name }),
        removed ? "success" : "",
      );
    } catch (error) {
      showToast(String(error));
    }
  }

  function loadSelectedPreset() {
    const preset = state.presets.find((entry) => entry.name === $("preset-select").value);
    if (!preset) return;
    writeSpec(preset.spec);
    $("launcher-dialog").showModal();
    void loadAgentCapabilities($("agent-input").value);
  }

  function openLauncher() {
    writeSpec(defaultSpec());
    void loadKnownProjects();
    $("launcher-dialog").showModal();
    $("cwd-input").focus();
    void loadAgentCapabilities($("agent-input").value);
  }

  function bindLauncherEvents() {
    $("agent-input").addEventListener("change", () => {
      syncAgentFields();
      void loadAgentCapabilities($("agent-input").value);
      schedulePreview();
    });
    [
      "cwd-input",
      "model-input",
      "name-input",
      "effort-input",
      "permission-input",
      "sandbox-input",
      "profile-input",
      "resume-input",
      "resume-id-input",
      "budget-input",
      "port-base-input",
      "port-count-input",
      "setup-hook-input",
      "teardown-hook-input",
      "prompt-input",
      "search-input",
    ].forEach((id) => {
      $(id).addEventListener("input", () => {
        if (id === "cwd-input") clearFolderValidation();
        if (id === "resume-input" || id === "model-input" || id === "effort-input") syncAgentFields();
        schedulePreview();
      });
      $(id).addEventListener("change", () => {
        if (id === "cwd-input") clearFolderValidation();
        if (id === "resume-input" || id === "model-input" || id === "effort-input") syncAgentFields();
        schedulePreview();
      });
    });
    $("pick-folder-button").addEventListener("click", async () => {
      let folder;
      try {
        folder = await invoke("pick_folder");
      } catch (error) {
        showToast(String(error));
        return;
      }
      if (folder) {
        $("cwd-input").value = folder;
        clearFolderValidation();
        schedulePreview();
        void loadProjectTemplates();
      }
    });
    $("cwd-input").addEventListener("change", () => void loadProjectTemplates());
    $("register-root-button").addEventListener("click", () => void registerProjectRoot());
    $("register-root-empty-button").addEventListener("click", () => void registerProjectRoot());
    $("project-root-add-button").addEventListener("click", () => void registerProjectRoot());
    $("project-root-list").addEventListener("click", (event) => {
      const button = event.target.closest("button[data-project-root-remove]");
      if (button) void removeProjectRoot(button.dataset.projectRootRemove);
    });
    $("project-select").addEventListener("change", () => {
      const path = $("project-select").value;
      if (!path) return;
      $("cwd-input").value = path;
      clearFolderValidation();
      schedulePreview();
      // The chosen project may declare its own templates; the folder changed
      // without the input's change event firing.
      void loadProjectTemplates();
    });
    $("template-select").addEventListener("change", () => applyProjectTemplate());
    $("pick-extra-button").addEventListener("click", async () => {
      let folders;
      try {
        folders = await invoke("pick_extra_dirs");
      } catch (error) {
        showToast(String(error));
        return;
      }
      if (folders?.length) {
        state.extraDirs = folders;
        $("extra-dirs-input").value = folders.join("; ");
        schedulePreview();
      }
    });
    $("save-preset-button").addEventListener("click", saveCurrentPreset);
    $("preset-select").addEventListener("change", () => {
      $("delete-preset-button").disabled = !$("preset-select").value;
    });
    $("delete-preset-button").addEventListener("click", () => void deleteSelectedPreset());
    $("launch-preset-button").addEventListener("click", loadSelectedPreset);
    $("restore-presets-button").addEventListener("click", async () => {
      try {
        const restored = await invoke("restore_builtin_presets");
        await loadPresets();
        showToast(
          restored ? t("presets-restored", { count: restored }) : t("presets-none-hidden"),
          restored ? "success" : "",
        );
      } catch (error) {
        showToast(String(error));
      }
    });
    $("launcher-form").addEventListener("submit", (event) => event.preventDefault());
    $("launch-button").addEventListener("click", () => void launchCurrentSpec());
  }

  return {
    bindEvents: bindLauncherEvents,
    defaultSpec,
    optionalCount,
    teamsChoice,
    commaList,
    readSpec,
    setPermissionValue,
    writeSpec,
    clearFolderValidation,
    showFolderValidation,
    capabilityForAgent,
    renderCapabilityFields,
    loadAgentCapabilities,
    syncAgentFields,
    schedulePreview,
    updatePreview,
    launchCurrentSpec,
    loadPresets,
    loadProjectTemplates,
    applyProjectTemplate,
    renderProjectRoots,
    loadProjectRoots,
    refreshScannedProjects,
    removeProjectRoot,
    loadKnownProjects,
    registerProjectRoot,
    saveCurrentPreset,
    deleteSelectedPreset,
    loadSelectedPreset,
    openLauncher,
  };
}
