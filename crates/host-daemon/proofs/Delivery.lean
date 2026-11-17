import Std

set_option warningAsError true

/-
Durable admission and delivery recovery for ONE command identity and its native
start plus optional session-detach work. replaySafe means checkpoint.capture.
The command's thread/type are fixed, as required by storage::commands::receipt.

The model abstracts a committed SQLite transaction as one step. reopen keeps
committed state; restart models executor::start_effects (recover_effects, then
runtime_restarted(false), before starting workers). worker denotes an opaque
process ownership token. Retry means the provider has confirmed NotSent; it is
not an allowed interpretation of an unknown delivery. resolve is an explicit
user cancellation of that unknown input. prepare records a successful native
allocation, not its I/O. Resolution detaches that binding and queues cleanup;
the lane stays occupied until that cleanup is settled. A successful settlement records
its completion; it does not prove what the external provider executed.

register repeats registration of the same already-admitted effect: neither its
ownership nor its completed result may be reset by that insertion.

These are model proofs, not proofs of SQLite, counter exhaustion, provider I/O, worker scheduling,
steering/restart writes, or application continuation after a confirmed native
turn. DeliveryCases and the SQLite conformance test check the real writer,
outbox, uncertainty decision and explicit resolution at this boundary.
-/
namespace Delivery

inductive Mode where
  | native
  | replaySafe
  deriving BEq, DecidableEq

inductive Phase where
  | absent
  | pending
  | running (worker : Nat)
  | unknown
  | succeeded
  | cancelled
  deriving BEq, DecidableEq

structure State where
  receipt : Option Bool := none
  phase : Phase := .absent
  attempts : Nat := 0
  evaluations : Nat := 0
  -- A remembered attached binding, including a stopped session after restart.
  prepared : Bool := false
  cleanup : Phase := .absent
  cleanupAttempts : Nat := 0

inductive Op where
  | submit (accepted : Bool) (commit : Bool)
  | register
  | prepare (worker : Nat)
  | claim (worker : Nat)
  | acknowledge (worker : Nat)
  | notSent (worker : Nat)
  | acknowledgeCleanup (worker : Nat)
  | retryCleanup (worker : Nat)
  | loseReply (worker : Nat)
  | resolve
  | restart
  | reopen

def submit (state : State) (accepted commit : Bool) : State × Bool :=
  match state.receipt with
  | some result => (state, result)
  | none =>
    let evaluated := { state with evaluations := state.evaluations + 1 }
    if commit then
      ({ evaluated with receipt := some accepted, phase := if accepted then .pending else .absent }, accepted)
    else (evaluated, false)

def step (mode : Mode) (state : State) (op : Op) : State × Bool :=
  match op with
  | .submit accepted commit => submit state accepted commit
  | .register => (state, state.receipt == some true)
  | .prepare worker =>
    if mode = .native ∧ state.phase = .running worker then
      ({ state with prepared := true }, true)
    else (state, false)
  | .claim worker =>
    if state.phase = .pending then
      ({ state with phase := .running worker, attempts := state.attempts + 1 }, true)
    else if state.cleanup = .pending then
      ({ state with cleanup := .running worker, cleanupAttempts := state.cleanupAttempts + 1 }, true)
    else (state, false)
  | .acknowledge worker =>
    if state.phase = .running worker then ({ state with phase := .succeeded }, true)
    else (state, false)
  | .notSent worker =>
    if state.phase = .running worker then ({ state with phase := .pending }, true)
    else (state, false)
  | .acknowledgeCleanup worker =>
    if state.cleanup = .running worker then ({ state with cleanup := .succeeded }, true)
    else (state, false)
  | .retryCleanup worker =>
    if state.cleanup = .running worker then ({ state with cleanup := .pending }, true)
    else (state, false)
  | .loseReply worker =>
    if mode = .native ∧ state.phase = .running worker then
      ({ state with phase := .unknown }, true)
    else (state, false)
  | .resolve =>
    if mode = .native ∧ state.phase = .unknown then
      ({ state with
        phase := .cancelled
        prepared := false
        cleanup := if state.prepared then .pending else .absent }, true)
    else (state, false)
  | .restart =>
    let phase := match mode, state.phase with
      | .native, .pending => .cancelled
      | .native, .running _ => .unknown
      | .replaySafe, .running _ => .pending
      | _, phase => phase
    let cleanup := match state.cleanup with
      | .running _ => .pending
      | phase => phase
    ({ state with phase, cleanup }, true)
  | .reopen => (state, true)

def replay (mode : Mode) (state : State) (ops : List Op) : State :=
  ops.foldl (fun state op => (step mode state op).1) state

def Valid (state : State) : Prop :=
  state.receipt = some true ↔ state.phase ≠ .absent

def laneOpen (state : State) : Bool :=
  let idle := fun phase : Phase => match phase with
  | .absent | .succeeded | .cancelled => true
  | _ => false
  idle state.phase && idle state.cleanup

theorem initial_valid : Valid {} := by simp [Valid]

theorem step_preserves_valid (mode : Mode) (state : State) (op : Op) (valid : Valid state) :
    Valid (step mode state op).1 := by
  cases mode <;> cases op <;> cases receipt : state.receipt with
  | none =>
    cases phase : state.phase <;> simp_all [Valid, step, submit] <;>
      split <;> simp_all <;> split <;> simp_all
  | some accepted =>
    cases accepted <;> cases phase : state.phase <;> simp_all [Valid, step, submit] <;>
      split <;> simp_all <;> split <;> simp_all

theorem replay_preserves_valid (mode : Mode) (state : State) (ops : List Op) (valid : Valid state) :
    Valid (replay mode state ops) := by
  induction ops generalizing state with
  | nil => exact valid
  | cons op ops ih => exact ih _ (step_preserves_valid mode state op valid)

theorem receipted_submit_is_not_reevaluated (state : State) (result accepted commit : Bool)
    (saved : state.receipt = some result) :
    submit state accepted commit = (state, result) := by
  simp [submit, saved]

theorem failed_commit_changes_no_durable_state (state : State) (accepted : Bool)
    (missing : state.receipt = none) :
    let next := (submit state accepted false).1
    next.receipt = state.receipt ∧ next.phase = state.phase ∧ next.attempts = state.attempts ∧
      next.prepared = state.prepared ∧ next.cleanup = state.cleanup ∧
      next.cleanupAttempts = state.cleanupAttempts := by
  simp [submit, missing]

theorem receipt_and_evaluations_survive_step (mode : Mode) (state : State) (op : Op)
    (result : Bool) (saved : state.receipt = some result) :
    (step mode state op).1.receipt = some result ∧
      (step mode state op).1.evaluations = state.evaluations := by
  cases op <;> simp_all [step, submit] <;> split <;> (try simp_all) <;> split <;> simp_all

theorem receipt_and_evaluations_survive_replay (mode : Mode) (state : State) (ops : List Op)
    (result : Bool) (saved : state.receipt = some result) :
    (replay mode state ops).receipt = some result ∧
      (replay mode state ops).evaluations = state.evaluations := by
  induction ops generalizing state with
  | nil => exact ⟨saved, rfl⟩
  | cons op ops ih =>
    obtain ⟨receipt, evaluations⟩ := receipt_and_evaluations_survive_step mode state op result saved
    obtain ⟨finalReceipt, finalEvaluations⟩ := ih _ receipt
    exact ⟨finalReceipt, finalEvaluations.trans evaluations⟩

theorem claim_requires_pending (mode : Mode) (state : State) (worker : Nat)
    (claimed : (step mode state (.claim worker)).2 = true) :
    state.phase = .pending ∨ state.cleanup = .pending := by
  simp only [step] at claimed
  split at claimed
  · exact Or.inl (by assumption)
  · split at claimed
    · exact Or.inr (by assumption)
    · simp at claimed

theorem pending_input_is_claimed_before_cleanup (mode : Mode) (state : State) (worker : Nat)
    (pending : state.phase = .pending) :
      (step mode state (.claim worker)).1.phase = .running worker ∧
      (step mode state (.claim worker)).1.attempts = state.attempts + 1 := by
  simp [step, pending]

theorem stale_acknowledgement_is_noop (mode : Mode) (state : State) (worker : Nat)
    (stale : state.phase ≠ .running worker) :
    step mode state (.acknowledge worker) = (state, false) := by
  simp [step, stale]

theorem stale_not_sent_cannot_release_lane (mode : Mode) (state : State) (worker : Nat)
    (stale : state.phase ≠ .running worker) :
    step mode state (.notSent worker) = (state, false) := by
  simp [step, stale]

theorem stale_uncertainty_cannot_replace_newer_work (mode : Mode) (state : State) (worker : Nat)
    (stale : state.phase ≠ .running worker) :
    step mode state (.loseReply worker) = (state, false) := by
  simp [step, stale]

theorem unknown_survives_automatic_step (state : State) (op : Op)
    (valid : Valid state) (unknown : state.phase = .unknown) (automatic : op ≠ .resolve) :
    (step .native state op).1.phase = .unknown ∧
      (step .native state op).1.attempts = state.attempts := by
  have saved : state.receipt = some true := valid.mpr (by simp [unknown])
  cases op <;> simp_all [step, submit] <;> split <;> simp_all

theorem unknown_survives_arbitrary_automatic_replay (state : State) (ops : List Op)
    (valid : Valid state) (unknown : state.phase = .unknown)
    (automatic : ∀ op ∈ ops, op ≠ .resolve) :
    (replay .native state ops).phase = .unknown ∧
      (replay .native state ops).attempts = state.attempts := by
  induction ops generalizing state with
  | nil => exact ⟨unknown, rfl⟩
  | cons op ops ih =>
    obtain ⟨phase, attempts⟩ := unknown_survives_automatic_step state op valid unknown
      (automatic op (by simp))
    obtain ⟨finalPhase, finalAttempts⟩ := ih _ (step_preserves_valid .native state op valid)
      phase (by intro action member; exact automatic action (by simp [member]))
    exact ⟨finalPhase, finalAttempts.trans attempts⟩

theorem restart_fences_native_worker (state : State) (worker : Nat)
    (running : state.phase = .running worker) :
    (step .native state .restart).1.phase = .unknown ∧
      step .native (step .native state .restart).1 (.acknowledge worker) =
        ((step .native state .restart).1, false) := by
  simp [step, running]

theorem unsent_native_start_is_cancelled_on_restart (state : State)
    (pending : state.phase = .pending) :
    (step .native state .restart).1.phase = .cancelled := by
  simp [step, pending]

theorem replay_safe_work_can_be_reclaimed (state : State) (oldWorker newWorker : Nat)
    (running : state.phase = .running oldWorker) :
    (step .replaySafe (step .replaySafe state .restart).1 (.claim newWorker)).2 = true ∧
      (step .replaySafe (step .replaySafe state .restart).1 (.claim newWorker)).1.phase =
        .running newWorker := by
  simp [step, running]

theorem reclaimed_safe_work_rejects_old_worker (state : State) (oldWorker newWorker : Nat)
    (running : state.phase = .running oldWorker) (fresh : newWorker ≠ oldWorker) :
    let reclaimed := (step .replaySafe (step .replaySafe state .restart).1 (.claim newWorker)).1
    step .replaySafe reclaimed (.acknowledge oldWorker) = (reclaimed, false) := by
  simp [step, running, fresh]

theorem explicit_resolution_releases_sessionless_unknown_lane (state : State)
    (unprepared : state.prepared = false)
    (unknown : state.phase = .unknown) :
    laneOpen state = false ∧ (step .native state .resolve).2 = true ∧
      laneOpen (step .native state .resolve).1 = true := by
  simp [laneOpen, step, unknown, unprepared]

theorem prepared_resolution_waits_for_cleanup (state : State)
    (prepared : state.prepared = true) (unknown : state.phase = .unknown) :
    (step .native state .resolve).1.phase = .cancelled ∧
      (step .native state .resolve).1.cleanup = .pending ∧
      laneOpen (step .native state .resolve).1 = false := by
  simp [step, laneOpen, prepared, unknown]

theorem cleanup_completion_releases_lane (state : State) (worker : Nat)
    (prepared : state.prepared = true) (unknown : state.phase = .unknown) :
    let resolved := (step .native state .resolve).1
    let claimed := (step .native resolved (.claim worker)).1
    (step .native claimed (.acknowledgeCleanup worker)).2 = true ∧
      laneOpen (step .native claimed (.acknowledgeCleanup worker)).1 = true := by
  simp [step, laneOpen, prepared, unknown]

#print axioms replay_preserves_valid
#print axioms receipt_and_evaluations_survive_replay
#print axioms claim_requires_pending
#print axioms pending_input_is_claimed_before_cleanup
#print axioms stale_acknowledgement_is_noop
#print axioms stale_not_sent_cannot_release_lane
#print axioms stale_uncertainty_cannot_replace_newer_work
#print axioms unknown_survives_arbitrary_automatic_replay
#print axioms restart_fences_native_worker
#print axioms unsent_native_start_is_cancelled_on_restart
#print axioms replay_safe_work_can_be_reclaimed
#print axioms reclaimed_safe_work_rejects_old_worker
#print axioms explicit_resolution_releases_sessionless_unknown_lane
#print axioms prepared_resolution_waits_for_cleanup
#print axioms cleanup_completion_releases_lane

end Delivery
