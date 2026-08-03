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
label-what-will-run = WHAT WILL RUN

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
