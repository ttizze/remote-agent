import Delivery
import Lean

set_option warningAsError true

open Delivery

structure Action where
  name : String
  worker : Nat := 0
  accepted : Bool := true
  commit : Bool := true
  deriving Lean.ToJson

def action : Op → Action
  | .submit accepted commit => { name := "submit", accepted, commit }
  | .register => { name := "register" }
  | .prepare worker => { name := "prepare", worker }
  | .claim worker => { name := "claim", worker }
  | .acknowledge worker => { name := "acknowledge", worker }
  | .notSent worker => { name := "notSent", worker }
  | .acknowledgeCleanup worker => { name := "acknowledgeCleanup", worker }
  | .retryCleanup worker => { name := "retryCleanup", worker }
  | .loseReply worker => { name := "loseReply", worker }
  | .resolve => { name := "resolve" }
  | .restart => { name := "restart" }
  | .reopen => { name := "reopen" }

structure Job where
  phase : String
  worker : Option Nat
  attempts : Nat

  deriving Lean.ToJson

structure Snapshot where
  receipt : Option Bool
  input : Job
  cleanup : Job
  evaluations : Nat
  prepared : Bool
  uncertain : Bool
  laneOpen : Bool
  deriving Lean.ToJson

def job (phase : Phase) (attempts : Nat) : Job where
  phase := match phase with
    | .absent => "absent"
    | .pending => "pending"
    | .running _ => "running"
    | .unknown => "unknown"
    | .succeeded => "succeeded"
    | .cancelled => "cancelled"
  worker := match phase with
    | .running worker => some worker
    | _ => none
  attempts

def snapshot (state : State) : Snapshot where
  receipt := state.receipt
  input := job state.phase state.attempts
  cleanup := job state.cleanup state.cleanupAttempts
  evaluations := state.evaluations
  prepared := state.prepared
  uncertain := state.phase == .unknown
  laneOpen := Delivery.laneOpen state

structure CaseStep where
  operation : Action
  ok : Bool
  state : Snapshot
  deriving Lean.ToJson

structure Trace where
  mode : String
  steps : List CaseStep
  deriving Lean.ToJson

def operations : List Op := [
  .submit true true, .submit false true, .submit true false, .register,
  .prepare 0, .prepare 1, .claim 0, .claim 1, .acknowledge 0, .acknowledge 1,
  .notSent 0, .notSent 1, .loseReply 0, .loseReply 1,
  .acknowledgeCleanup 0, .acknowledgeCleanup 1, .retryCleanup 0, .retryCleanup 1,
  .resolve, .restart, .reopen
]

def extend (mode : Mode) (state : State) (steps : List CaseStep) (op : Op) : State × List CaseStep :=
  let (next, ok) := step mode state op
  (next, steps ++ [{ operation := action op, ok, state := snapshot next }])

def controlKey (state : State) : Option Bool × Phase × Bool × Phase :=
  (state.receipt, state.phase, state.prepared, state.cleanup)

/- Explore every outgoing edge of each control state reached within eight steps,
   retaining one witness prefix per control state. Counters are checked after
   every step, but are omitted from this key; cycle witnesses below exercise
   repeated retries and recovery. The inductive proofs have no length bound;
   this finite corpus is NOT exhaustive over all operation sequences.
   Each emitted trace starts on its own real SQLite database. -/
def main : IO Unit := do
  let stdout ← IO.getStdout
  for mode in [Mode.native, Mode.replaySafe] do
    let label := if mode == .native then "native" else "replaySafe"
    let mut seen := [controlKey {}]
    let mut frontier : List (State × List CaseStep) := [({}, [])]
    for _ in List.range 8 do
      let mut upcoming := []
      for (state, steps) in frontier do
        for op in operations do
          let (next, extended) := extend mode state steps op
          stdout.putStrLn (Lean.toJson (Trace.mk label extended)).compress
          if !seen.contains (controlKey next) then
            seen := controlKey next :: seen
            upcoming := (next, extended) :: upcoming
      frontier := upcoming
    for ops in [
      [.submit true false, .reopen, .submit true true, .submit false true, .claim 0,
       .loseReply 0, .reopen, .restart, .restart, .acknowledge 0, .submit true true,
       .resolve, .reopen, .claim 1],
      [.submit true true, .claim 0, .notSent 1, .notSent 0, .claim 1,
       .restart, .acknowledge 0, .claim 0, .acknowledge 1, .reopen],
      [.submit true true, .claim 0, .prepare 0, .loseReply 0, .reopen, .restart,
       .resolve, .reopen, .claim 1, .restart, .acknowledgeCleanup 1, .claim 0,
       .retryCleanup 1, .retryCleanup 0, .claim 1, .acknowledgeCleanup 0,
       .acknowledgeCleanup 1, .reopen, .register, .claim 0]
    ] do
      let (_, steps) := ops.foldl (fun (state, steps) op => extend mode state steps op) ({}, [])
      stdout.putStrLn (Lean.toJson (Trace.mk label steps)).compress
