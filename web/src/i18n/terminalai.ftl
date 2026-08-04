# The one catalog shared by the Rust control plane and the web renderer.
# Keep row status labels short: the row uses the glyph as its stable visual
# affordance and puts this value in the tooltip/accessible name.

brand-subtitle = fleet control plane
button-presets = Presets
button-refresh = Refresh
button-check-updates = Check updates
button-show-model-effort-cost = Show model, effort, and cost
button-show-needs-input = Needs input
button-review = Review
button-fleet = Fleet
button-apply = Apply
button-close = Close
button-cancel = Cancel
button-launch-session = Launch session
button-save-preset = Save preset
button-fix = Fix
button-fix-unavailable = Fix unavailable
button-recheck = Recheck
button-preflight = Preflight checks
button-send-reply = Send reply
button-preflight-short = Preflight
button-new-session = New session
button-wide = Wide
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

preflight-ready = Ready
preflight-needs-attention = Needs attention
preflight-unavailable = Unavailable
preflight-not-applicable = Not applicable
preflight-checking = Checking local dependencies…
preflight-all-ready = All detected control-plane dependencies are ready.

external-running = Running
external-ended = Ended
external-unknown = Unknown

count-session-one = { $count } session
count-session-other = { $count } sessions
count-queued-one = { $count } queued
count-queued-other = { $count } queued
count-needs-you-one = { $count } needs you
count-needs-you-other = { $count } need you
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
empty-first-launch = Launch your first session
empty-first-root = Or register the folder that holds your repositories, and every project in it becomes one click away.
empty-first-explainer = How this works
dwell-explained = How long this session has been in its current state
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
work-started = Queued { $count } { $count ->
    [one] project
   *[other] projects
  }
work-outcome = { $done } done · { $running } running · { $pending } waiting · { $flagged } flagged · { $failed } failed · { $skipped } skipped
work-run-paused = Run paused
work-state-pending = waiting for a slot
work-state-running = running
work-state-done = done
work-state-failed = failed
work-state-skipped = skipped
work-state-flagged = uncommitted changes
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
broadcast-skip-not-running = not running
broadcast-sent = Sent to { $delivered } of { $total }
broadcast-refused = { $count } skipped
broadcast-empty-prompt = Type a prompt first
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
reason-unknown = No reason recorded

relative-now = just now
empty-no-focused-session = No focused session
empty-waiting-for-session = Waiting for a session
empty-no-output = No output yet
empty-no-transition-history = No transition history was persisted for this session.
empty-focus-diagnostics = Focus a session to inspect its status evidence.
empty-no-daemon-records = No daemon records have arrived yet.
empty-no-review = No sessions need review.
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
review-cost = cost { $cost }
preflight-title = Ready the control plane
preflight-footer = Fixes are local and reversible. Agent installation is left to its vendor.
terminal-waiting = Waiting for a session
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

store-quarantined = Session store quarantined
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

review-land = Land
review-landing = Landing…
review-land-hint = Apply this session's changes into its repository, or refuse and say why
review-landed = Landed { $files } file(s); review and commit when ready
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
button-group-list = Group the list by folder, agent, or status
label-filter-agent = Filter by agent
label-filter-status = Filter by status
filter-agent-all = All agents
filter-status-all = Any status
filter-status-attention = Needs you
filter-status-working = Working
filter-status-idle = Idle
filter-status-blocked = Rate limited
filter-status-exited = Exited
row-group = Group
