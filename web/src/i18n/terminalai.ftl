# The one catalog shared by the Rust control plane and the web renderer.
# Keep row status labels short: the row uses the glyph as its stable visual
# affordance and puts this value in the tooltip/accessible name.

brand-subtitle = fleet control plane
button-presets = Presets
button-refresh = Refresh
button-check-updates = Check updates
update-checking = Checking…
update-newest = TerminalAI v{ $version } is the newest published build; no update was installed.
update-http-error = GitHub returned HTTP { $status }
update-invalid-release = the latest release had no semantic version
update-available = TerminalAI v{ $latest } is available (installed v{ $current }). Nothing was installed automatically.
button-open-releases = Open releases page
update-up-to-date = TerminalAI v{ $version } is up to date; no update was installed.
update-failed = Update check failed: { $error }
button-show-model-effort-cost = Show model, effort, and cost
button-hide-model-effort-cost = Hide model, effort, and cost
button-show-needs-input = Needs input
button-review = Review
button-fleet = Fleet
button-apply = Apply
button-close = Close
button-cancel = Cancel
button-launch-session = Launch session
button-save-preset = Save preset
button-delete-preset = Delete selected preset
button-delete-preset-title = Remove this saved or built-in preset
button-remove = Remove
button-prompts-short = Prompts
button-prompts-open = Open the prompt library
button-fix = Fix
button-fix-unavailable = Fix unavailable
button-recheck = Recheck
button-preflight = Preflight checks
button-send-reply = Send reply
button-preflight-short = Preflight
button-new-session = New session
button-wide = Wide
button-compact = Compact
button-dismiss = Dismiss
button-recheck-all = Recheck all
button-back-to-fleet = Back to fleet
button-refit-terminal = Refit the terminal grid to the pane
button-clear-terminal = Clear rendered terminal
button-load-history = Load older output from disk
history-loaded = Loaded { $bytes } KB of earlier output
history-empty = No older output is stored for this session

label-session-fleet = Session fleet
label-filter-sessions = Filter sessions
label-tracked-sessions = Tracked sessions
label-focused-terminal = Focused terminal
label-status-counts = Fleet state counts
label-prerequisites = TerminalAI prerequisites
label-pending-review = Pending review
label-external-sessions = Sessions started outside TerminalAI
label-diagnostics = Session status diagnostics
label-daemon-logs = Daemon logs
label-live-fleet = LIVE FLEET
label-first-run-check = FIRST-RUN CHECK
label-pending-review-eyebrow = PENDING REVIEW
label-elsewhere = ELSEWHERE ON THIS MACHINE
label-new-controlled-session = NEW CONTROLLED SESSION
launcher-title = Launch an agent
label-what-will-run = WHAT WILL RUN
launcher-folder-required = Choose a project folder before launching.

status-needs-approval = Needs approval
status-awaiting-input = Awaiting input
status-needs-you = Needs you
status-rate-limited = Rate limited
status-working = Working
status-thinking = Thinking
status-idle = Idle
status-starting = Starting
status-queued = Queued
status-unknown = State unknown
status-exited = Exited
status-preparing = Preparing environment
status-tearing-down = Tearing down environment
status-stalled = { $status } — stalled
status-stalled-detail = Has held this status for over { $minutes } minutes; it may be wedged rather than busy
status-unresponsive = { $status } — silent
status-unresponsive-detail = No output, transcript or hook event for over { $minutes } minutes. It has not been restarted — a silent agent may still be thinking
status-finished = Finished
status-failed = Failed after { $restarts } restarts
status-finished-detail = The agent ended its own session; it will not be restarted
status-failed-detail = The supervisor gave up after { $restarts } restarts and will not try again
status-failed-detail-code = The supervisor gave up after { $restarts } restarts; last exit code { $code }

preflight-ready = Ready
preflight-needs-attention = Needs attention
preflight-unavailable = Unavailable
preflight-blocked = Blocked by policy
preflight-not-applicable = Not applicable
preflight-checking = Checking local dependencies…
preflight-all-ready = All detected control-plane dependencies are ready.
preflight-run-error = Preflight could not run: { $error }
preflight-fix-applied = { $id } preflight fix applied
preflight-action-error = Could not { $action } { $id }: { $error }

external-running = Running
external-ended = Ended
external-unknown = Unknown
external-process-id = Process id
external-same-folder = TerminalAI also supervises a session in this folder
external-same-folder-short = same folder
external-load-error = Could not read external sessions: { $error }
external-reported-by-agent = Reported by the agent itself, in its own words
external-blocked-on = { $state } on { $waiting }

count-session-one = { $count } session
count-session-other = { $count } sessions
count-queued-one = { $count } queued
count-queued-other = { $count } queued
count-needs-attention-one = { $count } needs attention
count-needs-attention-other = { $count } need attention
count-active-one = { $count } active
count-active-other = { $count } active
count-rate-limited-one = { $count } rate limited
count-rate-limited-other = { $count } rate limited
count-check-one = { $count } check needs attention before the fleet can be trusted.
count-check-other = { $count } checks need attention before the fleet can be trusted.
count-pending-one = { $count } pending
count-pending-other = { $count } pending
count-conflict-one = { $count } with conflicts
count-conflict-other = { $count } with conflicts
count-timed-out-one = { $count } timed out
count-timed-out-other = { $count } timed out
count-changed-priority-one = { $count } session changed priority
count-changed-priority-other = { $count } sessions changed priority
count-file-one = { $count } file
count-file-other = { $count } files
count-external-one = { $count } not supervised
count-external-other = { $count } not supervised
count-unknown-external-one = { $count } unknown
count-unknown-external-other = { $count } unknown

fleet-live = live
fleet-spent = spent
rollup-title = Fleet spend
explainer-title = How the fleet screen works
button-explainer-title = How the fleet screen works
explainer-rows-heading = Rows are not terminals
explainer-rows = Each row is a compact status line, not a terminal. That is what lets around thirty sessions share one screen — a terminal needs a lot of space, and a status line needs 28 pixels.
explainer-focus-heading = One session at a time gets a terminal
explainer-focus = Clicking a row focuses it and attaches the terminal below. Pin up to three more to watch them live alongside it. Everything else keeps running and keeps reporting.
explainer-attention-heading = The fleet tells you when it needs you
explainer-attention = Sessions sort by how much they need you. When one asks a question or wants permission, it moves to the top and you can answer straight from its row without focusing it.
explainer-states-heading = What the states mean
empty-first-root = Or register the folder that holds your repositories, and every project in it becomes one click away.
empty-first-explainer = How this works
dwell-explained = How long this session has been in its current state
answer-deadline = answers itself in { $seconds }s
answer-deadline-passed = answered itself
answer-deadline-explained = The agent proceeds without you 60 seconds after asking, so this is how long is left to reply
status-needs-approval-explained = The agent wants permission before it does something
status-awaiting-input-explained = The agent asked you a question and is waiting
status-needs-you-explained = The agent stopped and wants your attention
status-rate-limited-explained = The provider is refusing requests until its limit resets
status-working-explained = Running commands or editing files
status-thinking-explained = Working out what to do next
status-idle-explained = Finished its turn and waiting for you
status-starting-explained = Launching — the agent has not reported in yet
status-queued-explained = Waiting for a free slot in the fleet
status-unknown-explained = No status reported yet
status-exited-explained = The process has ended
projects-title = Projects
work-prompt = Run this prompt
work-start = Run across listed projects
work-pause = Pause run
work-resume = Resume run
work-clear = Clear run
work-no-prompts = No stored prompts yet — add one in your prompt library
prompt-library-title = Prompt library
prompt-library-count = { $count } stored { $count ->
    [one] prompt
   *[other] prompts
  }
prompt-library-unavailable = Prompt library unavailable
prompt-library-load-error = Could not read prompt library: { $error }
prompt-library-empty = No stored prompts yet. Create one here.
prompt-editor-title = EDIT PROMPT
prompt-name = Prompt name
prompt-name-hint = A short name appears in the work queue.
prompt-name-placeholder = Name this prompt
prompt-text = Prompt text
prompt-text-placeholder = Write the reusable instruction here
prompt-new = New prompt
prompt-save = Save prompt
prompt-select = Edit { $name }
prompt-delete = Delete { $name }
prompt-source-seeded = Seeded from a local prompt file
prompt-source-local = Saved in TerminalAI
prompt-name-required = Name the prompt first
prompt-text-required = Write the prompt first
prompt-saved = Saved { $name }
prompt-deleted = Deleted { $name }
prompt-not-found = Prompt { $name } was already removed
work-started = Queued { $count } { $count ->
    [one] project
   *[other] projects
  }
work-outcome = { $done } done · { $running } running · { $pending } waiting · { $flagged } flagged · { $failed } failed · { $skipped } skipped · { $expired } expired
work-run-paused = Run paused
work-state-pending = waiting for a slot
work-state-running = running
work-state-done = done
work-state-failed = failed
work-state-skipped = skipped
work-state-flagged = uncommitted changes
work-state-expired = waited too long
work-expired-detail = Never got a fleet slot within { $minutes } minutes, so it was given up on rather than started against a tree that has since moved
work-approve = Run anyway
work-skip = Skip
work-dirty-detail = { $count } uncommitted { $count ->
    [one] change
   *[other] changes
  }
work-tree-unknown = Git could not read this tree
label-queue = Prompt queue for this session
action-queue = Queue prompts for { $name }
queue-count-title = { $count } queued for { $name }
queue-paused-title = { $name }'s queue is paused — { $reason }
queue-title = Prompt queue · { $name }
queue-title-generic = Prompt queue
queue-running = Sends the next prompt when this run finishes
queue-paused-detail = Paused — { $reason }
queue-pause-needs_approval = the run ended waiting for a permission decision
queue-pause-awaiting_input = the run ended asking a question
queue-pause-not_running = the session is not running
queue-pause-focused_and_edited = focused and edited — defocus it or send explicitly
queue-pause-operator = you paused it
queue-empty = Nothing queued yet
queue-add = Add a prompt
queue-add-placeholder = Sent when the current run finishes
queue-add-button = Queue it
queue-pause = Pause queue
queue-resume = Resume queue
queue-position = { $position }.
queue-move-up = Move earlier
queue-move-down = Move later
queue-save = Save this edit
queue-saved = Prompt updated
queue-withdraw = Withdraw this prompt
queue-empty-prompt = Type a prompt first
button-retry = Retry
queue-unavailable = Queue unavailable
queue-load-error = Could not read prompt queue: { $error }
loading = Loading…
label-projects-dialog = Projects and their roadmaps
button-projects-short = Projects
button-projects-open = See which projects still have roadmap work
projects-open-only = Only projects with open roadmap items
projects-column-project = Project
projects-column-open = Open
projects-column-touched = Roadmap touched
projects-column-next = Next item
projects-no-roadmap = no roadmap
projects-unreadable = not a checklist
projects-none-registered = Register a root to see your projects here
projects-none-matching = No project has open roadmap items
projects-unavailable = Projects unavailable
projects-load-error = Could not read projects: { $error }
projects-summary = { $withWork } of { $total } have open items · { $unknown } unknown
projects-launch = Launch
session-history-title = Finished sessions
session-history-note = What this supervisor archived, newest first. Only the layout and the exact command are kept — never the output.
button-history-short = History
menu-more = More actions
menu-tools = Tools
menu-presets = Presets
menu-presets-select = Saved launcher presets
menu-preset-launch = Launch
menu-preset-delete = Delete
menu-preset-restore = Restore built-ins
menu-explainer = How this screen works
launcher-advanced = Advanced options
launcher-advanced-hint = model, permissions, sandbox, resume, budget, worktree, ports, hooks
launcher-permission-custom = { $mode } (not modelled here — passed through)
session-history-count = { $count } archived
session-history-empty = Nothing has been archived yet. A session appears here once it has stopped and you archive its row.
session-history-error = Could not read the session history: { $error }
session-history-column-session = Session
session-history-column-folder = Folder
session-history-column-command = Command
session-history-column-archived = Archived
session-history-relaunch = Launch again
session-history-relaunch-note = The launcher opens with the agent, label and folder from this session. The rest of its command is shown above but not restored — the archive keeps the command as text, not as settings.
worktrees-title = Leftover checkouts
worktrees-note = Private checkouts this tool created that no live session owns. A branch still holding work is listed and never removed here — take it up in Git, where you can see the commits first.
worktrees-count = { $count } left over
worktrees-empty = No leftover checkouts. Every worktree this tool created belongs to a live session.
worktrees-error = Could not survey the checkouts: { $error }
worktrees-column-branch = Branch
worktrees-column-repo = Repository
worktrees-column-state = State
worktrees-state-merged = Fully merged
worktrees-state-unmerged = { $commits } unmerged commits
worktrees-state-unknown = Unknown: { $detail }
worktrees-missing-directory = registration only — the directory is gone
worktrees-remove = Remove
worktrees-removed = Checkout removed
projects-roots-title = Registered project roots
projects-root-add = Register root
projects-roots-empty = No project roots registered
projects-roots-load-error = Could not read project roots: { $error }
projects-root-remove = Remove { $root }
projects-root-removed = Removed project root { $root }
projects-root-not-found = Project root { $root } was already removed
touched-today = today
touched-days = { $days }d ago
touched-months = { $months }mo ago
label-project-folder = Project folder
label-known-projects = Known projects
button-browse = Browse
button-register-root = Register root
button-register-root-title = Register a folder that holds your repositories
project-choose = Choose a project…
projects-root-added = Registered { $root } — { $count } { $count ->
    [one] project found
   *[other] projects found
  }
projects-none-found = No Git repositories were found under { $root }
preset-builtin-mark = (built-in)
preset-name-required = Enter a preset name first
preset-saved = Preset “{ $name }” saved
preset-deleted = Deleted { $name }
preset-not-found = Preset { $name } was already removed
presets-load-error = Could not load presets: { $error }
button-restore-presets = Offer every built-in preset again
presets-restored = { $count } built-in { $count ->
    [one] preset restored
   *[other] presets restored
  }
presets-none-hidden = No built-in presets are hidden
template-label = This project's templates
template-none = Choose a template…
template-applied = Applied “{ $name }” from this project
template-unreadable = This project's templates could not be read: { $detail }
preview-folder = Choose a project folder to preview the exact command vector.
preview-waiting = Waiting for a valid folder
preview-resolving = Resolving native binary…
preview-exact = Exact argv preview
preview-refused = Launch refused
broadcast-title = Broadcast a prompt
label-broadcast = Broadcast a prompt to several sessions
broadcast-targets = Send to
broadcast-prompt = Prompt
broadcast-placeholder = Sent to every selected session
button-broadcast = Send to selected
button-broadcast-short = Broadcast
button-broadcast-open = Send one prompt to several sessions
broadcast-none-eligible = No session can take a prompt right now
broadcast-eligible = { $count } { $count ->
    [one] session can receive it
   *[other] sessions can receive it
  }
broadcast-skip-approval = waiting for a permission decision — answer it directly
broadcast-skip-focused-edited = focused and edited — defocus it or send explicitly
broadcast-skip-not-running = not running
broadcast-sent = Sent to { $delivered } of { $total }
broadcast-refused = { $count } skipped
broadcast-empty-prompt = Type a prompt first
broadcast-error = Could not broadcast: { $error }
focus-session-error = Could not focus session: { $error }
link-opened = Opened { $host }
event-stream-unavailable = Event stream unavailable: { $error }
terminal-refitted = Terminal refitted to { $cols } × { $rows }
label-rollup = Fleet cost and token rollup
button-open-rollup = Break down what the fleet has spent
rollup-empty = No sessions yet
rollup-complete = All { $priced } sessions priced
rollup-partial = { $priced } priced, { $unpriced } not yet read
rollup-by-agent = By agent
rollup-by-folder = By project folder
rollup-by-session = By session
rollup-total = Fleet total
rollup-requests = requests
tokens-input = Input
tokens-output = Output
tokens-cache-read = Cache read
tokens-cache-write = Cache write
tracked-sessions = { $count } tracked
event-drops = { $count } event drops
pricing-reporting = Prices as of { $pricing }; { $reporting } of { $sessions } sessions reporting
pricing-none = No session has reported a cost yet. Prices as of { $pricing }
pricing-age-current = The price table is { $days } days old.
pricing-age-stale = The price table is { $days } days old, past the { $threshold }-day mark — these figures were computed against rates that may have moved. Nothing is fetched at runtime; refresh the vendored table to update them.
pricing-age-undated = The price table carries no date, so it cannot be aged. This is the built-in fallback, used when the vendored snapshot could not be read.
spend-ceiling-of = Fleet spend { $spent } of { $ceiling }{ $window }.
spend-window-hours = { $hours ->
    [one] over the last hour
   *[other] over the last { $hours } hours
}
spend-ceiling-blocking = The ceiling is reached, so nothing new is starting; running sessions are untouched.
spend-no-ceiling = No fleet spend ceiling is set.
memory-explained = Private commit for this session's process tree
auth-expired = Agent sign-in expired
settings-title = Fleet limits
button-settings-short = Limits
button-save-settings = Apply
settings-note = Applied to the running daemon straight away. A limit refuses new sessions; it never stops one that is already running. Leave a field empty for no limit.
settings-max-live = Live sessions
settings-default-budget = Default session budget (USD)
settings-spend-ceiling = Fleet spend ceiling (USD)
settings-spend-window = Spend window (hours)
settings-memory-budget = Fleet memory budget (MB)
settings-memory-cap = Per-session memory cap (MB)
settings-max-processes = Processes per session
settings-saved = Fleet limits applied
settings-from-environment = Started from { $names }; changing a value here overrides it until the daemon restarts.
auth-expired-detail = { $agents } reported that it is not signed in. Queued work is held until you sign in again; running sessions are untouched.
memory-limited-explained = This session reached its memory cap; its allocations are being refused
spend-enforced-agents = A per-session budget is enforced for: { $agents }. Other agents are admission-refused only.
spend-enforced-none = No agent enforces a per-session budget; the ceiling refuses new sessions only.
announcement-one = { $name } needs you: { $status }.
announcement-many = { $count } sessions need you: { $names }.

sessions-count = { $count ->
    [one] { $count } session
   *[other] { $count } sessions
}

reason-session-created = Session created
reason-admission-queued = Waiting for an admission slot
reason-admission-granted = Admission slot granted
reason-agent-hook = Agent hook moved the session to { $status }
reason-app-server-event = Agent server moved the session to { $status }
reason-transcript-event = Transcript activity moved the session to { $status }
reason-pty-output = Terminal output moved the session to { $status }
reason-process-started = Process started
reason-process-exited = Process exited with code { $code }
reason-process-query = Process health was checked
reason-supervisor = Supervisor update
reason-manual = Operator action
reason-restored = Restored from the session store
reason-status-changed = State changed to { $status }
reason-context-compacting = Compacting the context window
reason-context-compacted = Context window compacted
reason-unknown = No reason recorded

context-explained = { $used } of { $window } tokens in the context window ({ $percent }% full)
context-no-window = { $used } tokens in the context window; this agent does not report its window size, so there is no percentage to show
context-unmeasured = No context reading yet for this session
button-approvals-short = Approvals
approvals-title = Approvals
approvals-note = Every session waiting on you, the one that has been waiting longest first. Nothing here approves anything on your behalf: there is no approve-all and no bypass mode, and what you type is sent to that session's prompt exactly as if you had typed it there. A session whose request cannot be described is still listed — that is the one to go and look at.
approvals-empty = No session is waiting on a decision.
approvals-unknown-request = This session is waiting, but did not say what for. Focus it to read the prompt.
approvals-answer-placeholder = Answer this session's prompt
approvals-answer-for = Answer the prompt in { $name }
approvals-send = Send
approvals-sent = Answer sent to the session
approvals-count = { $count ->
    [one] 1 waiting
   *[other] { $count } waiting
}

button-search-short = Search
button-working-sets-short = Layouts
working-sets-title = Working sets
working-sets-note = A named layout of many sessions, relaunched as one action. Restoring starts each session through the ordinary launch path, so the admission gate, the memory budget, the spend ceiling and the dirty-tree refusal all still apply — a session they refuse is reported, not forced. A private checkout is created fresh; a layout never adopts the one the original session had.
working-sets-name-placeholder = Name this layout
working-sets-save = Save the current fleet
working-sets-restore = Restore
working-sets-delete = Delete
working-sets-needs-name = Give the layout a name first.
working-sets-empty = No layouts saved yet. Name one and save the fleet you have now.
working-sets-saved = Saved "{ $name }" with { $count } session(s).
working-sets-started = started
working-sets-queued = queued by the admission gate
working-sets-restored = { $started } started, { $queued } queued, { $refused } refused.
working-sets-count = { $count ->
    [one] 1 layout
   *[other] { $count } layouts
}
working-sets-members = { $count ->
    [one] 1 session, { $pinned } pinned
   *[other] { $count } sessions, { $pinned } pinned
}
fleet-search-title = Search the fleet
fleet-search-note = Searches the output each session still has on disk, which is a bounded tail — the beginning of a long run is genuinely gone. Colour and cursor sequences are removed before matching, so what is searched is what was legible.
fleet-search-placeholder = Find in every session
fleet-search-case = Match case
fleet-search-run = Search
fleet-search-too-short = Type at least two characters. A shorter search matches most of every transcript and costs a read of the whole fleet to say so.
fleet-search-none = Nothing in the retained output matched "{ $needle }".
fleet-search-summary = { $sessions ->
    [one] 1 session, { $total } match(es)
   *[other] { $sessions } sessions, { $total } match(es)
}
fleet-search-hits = { $count ->
    [one] 1 match
   *[other] { $count } matches
}
fleet-search-truncated = Excerpts stop here; the count above is still the whole total.

find-placeholder = Find in this pane
find-next = Next match
find-previous = Previous match
find-close = Close find
find-searching = searching…
find-none = no matches
find-position = { $index } of { $total }

context-compactions = { $count ->
    [one] Compacted once so far
   *[other] Compacted { $count } times so far
}

relative-now = just now
empty-no-focused-session = No focused session
empty-waiting-for-session = Waiting for a session
empty-no-output = No output yet
empty-no-transition-history = No transition history was persisted for this session.
empty-focus-diagnostics = Focus a session to inspect its status evidence.
empty-no-daemon-records = No daemon records have arrived yet.
empty-no-sessions = No sessions yet
empty-no-sessions-detail = Launch a Claude Code or Codex session and its attention state will stay here.
empty-launch-first = Launch your first session
empty-terminal-focus = Focus a fleet row to attach the terminal
empty-terminal-detail = Only one xterm renderer is kept alive. Background sessions remain compact.
loading-fleet = Loading fleet…
review-no-pending-diffs = No pending diffs
review-unavailable = Review unavailable
review-load-error = Could not read review snapshot: { $error }
review-status-timed-out = Timed out
review-status-reviewed = Reviewed
review-status-pending = Pending
review-conflict-markers = Conflict markers surfaced
review-conflicted-file-one = { $count } conflicted file
review-conflicted-file-other = { $count } conflicted files
review-marker-lines = { $count } marker lines
review-show-diff = Show diff
review-truncated = truncated
review-no-diff = No textual diff was returned.
review-mark-reviewed = Mark reviewed
review-reviewed = Reviewed
review-marked = Session marked reviewed
review-mark-error = Could not mark session reviewed: { $error }
review-cost = cost { $cost }
preflight-title = Ready the control plane
preflight-footer = Fixes are local and reversible. Agent installation is left to its vendor.
terminal-waiting = Waiting for a session
terminal-status-detail = { $status } · { $dwell } · { $agent }
screen-reader-toggle = Toggle screen reader mode
screen-reader-enable = Enable screen reader mode (disables right-click copy and paste)
screen-reader-disable = Disable screen reader mode

diagnostics-why-this-state = WHY THIS STATE
diagnostics-current-status = Current status
diagnostics-from = from { $status }
diagnostics-source = source { $source }
diagnostics-for = for { $dwell }
source-launch = Launch
source-admission = Admission
source-hook = Hook
source-app-server = App server
source-transcript = Transcript
source-pty-output = Terminal output
source-process-start = Process start
source-process-exit = Process exit
source-process-query = Process query
source-supervisor = Supervisor
source-manual = Manual
source-restore = Restore
source-unknown = Unknown
logs-control-plane = CONTROL-PLANE LOG
logs-daemon-records = Daemon records
logs-latest-retained = Latest 256 records · daily files are retained under LocalAppData

action-focus-terminal = Focus terminal
action-focus-session = Focus { $name } terminal
action-pin = Pin
action-unpin = Unpin
action-revive = Revive { $name } with native resume
action-archive = Archive { $name }
action-archive-stopped = Archive stopped session
action-stop = Stop { $name }
action-cancel-queued = Cancel queued session
action-reply = Reply to { $name }
action-send-reply = Send reply to { $name }
action-unread-attention = Unread attention
action-repository = Repository
action-branch = Branch
action-allocated-ports = Allocated ports
action-tool-progress = Tool progress
action-restart-count = Restart count
reply-sent = Reply sent
stop-signal-sent = Stop signal sent
resume-started = Native session resume started
archive-stopped = Stopped session archived
history-load-error = Could not load older output: { $error }
unknown-time = unknown time

store-quarantined = Session store quarantined
store-write-failed = Fleet state is not being saved
store-write-failed-detail = The session store could not be written: { $error }. Rows are still live, but a restart would lose changes made since the last successful save.
store-quarantined-detail = The unreadable session store was moved to { $path }. New sessions start empty.
diagnostics-unavailable = Unavailable
session-created = Session created

rate-limit-resets-in = { $count ->
    [one] 1 session is rate limited; the soonest window reopens in { $minutes } min
   *[other] { $count } sessions are rate limited; the soonest window reopens in { $minutes } min
}
rate-limit-reset-unknown = { $count ->
    [one] 1 session is rate limited; it did not report a reset time
   *[other] { $count } sessions are rate limited; none reported a reset time
}
rate-limit-row = { $scope } quota
rate-limit-in-minutes = reopens in { $minutes } min
fleet-quota = { $percent }% quota
fleet-quota-unreported = quota —
quota-used = { $percent }% of the tightest quota window used
quota-reset-unreported = reset time not reported
quota-unreported = No agent has reported a quota window. Codex publishes one continuously; Claude Code reports only once a limit is hit.

review-land = Land
review-landing = Landing…
review-land-hint = Apply this session's changes into its repository, or refuse and say why
review-landed = Landed { $files } file(s); review and commit when ready
review-archive-on-land = Archive after landing
review-archive-on-land-title = Only after a landing succeeds. A worktree still holding unmerged commits is kept, never deleted.
review-landed-archived = Landed { $files } file(s) and archived the session; review and commit when ready
review-landed-not-archived = Landed { $files } file(s), but the session was not archived — { $reason }
review-land-refused = Landing refused — { $reason }
land-target-moved = the repository moved since this review (expected { $expected }, found { $found }); refresh and re-read the diff
land-target-dirty = the repository has uncommitted changes: { $paths }
land-conflict-markers = conflict markers are present in: { $paths }
land-patch-stale = the changes no longer apply cleanly: { $detail }
land-verify-failed = the verify command { $command } failed and the change was rolled back: { $output }
land-verify-not-reversed = the verify command { $command } failed AND the rollback failed ({ $error }); the repository needs manual repair
land-nothing = the session has no uncommitted changes
land-unavailable = the repository could not be read: { $detail }

pinned-waiting = Waiting for output…
label-pinned-split = Pinned session panes

group-none = No grouping
group-folder = By folder
group-agent = By agent
group-status = By status
button-group-list = Cycle grouping: none, folder, agent, or status
label-filter-agent = Filter by agent
label-filter-status = Filter by status
column-label-compact = STATUS / REPO
column-label-wide = STATUS / REPO · BRANCH
filter-agent-all = All agents
filter-status-all = Any status
filter-status-attention = Needs attention
filter-status-working = Working
filter-status-idle = Idle
filter-status-blocked = Rate limited
filter-status-exited = Exited
row-group = Group
