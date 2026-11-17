import Lean

set_option warningAsError true

namespace Recovery

structure Composer where
  text : String
  files : List String
  fresh : Bool
  deriving Lean.ToJson

def submitted : Composer := ⟨"sent", ["shared", "original"], true⟩
def emptyDraft : Composer := ⟨"", [], false⟩
def edited : Composer := ⟨"new typing", ["new", "shared"], false⟩

def recover (current : Composer) : Composer :=
  { text := if current.text.isEmpty then submitted.text else submitted.text ++ "\n" ++ current.text
    files := current.files ++ submitted.files.filter (fun file => !current.files.contains file)
    fresh := submitted.fresh || current.fresh }

inductive Phase where
  | sending | uncertain | accepted | saved (restore : Bool) | settled
  deriving DecidableEq

structure State where
  phase : Phase
  composer : Composer

def loseReply (state : State) : State :=
  if state.phase = .sending then { state with phase := .uncertain } else state

def finish (state : State) (restore : Bool) : State :=
  ⟨.settled, if restore then recover state.composer else state.composer⟩

def localReply (state : State) (accepted restore : Bool) : State :=
  if state.phase = .uncertain then
    if accepted then { state with phase := .accepted } else finish state restore
  else state

def savedReply (state : State) : State :=
  match state.phase with
  | .saved restore => finish state restore
  | _ => state

theorem lostReply_preserves_composer (state : State) :
    (loseReply state).composer = state.composer := by
  unfold loseReply
  split <;> rfl

theorem accepted_never_restores (draft : Composer) (restore : Bool) :
    (localReply ⟨.uncertain, draft⟩ true restore).composer = draft := by
  simp [localReply]

theorem saved_replay_is_idempotent (state : State) :
    savedReply (savedReply state) = savedReply state := by
  cases state with
  | mk phase composer => cases phase <;> rfl

structure Observation where
  phase : String
  composer : Composer
  deriving Lean.ToJson

def observe (state : State) : Observation :=
  ⟨match state.phase with
    | .sending => "sending"
    | .uncertain => "uncertain"
    | .accepted => "accepted"
    | .saved true => "restore"
    | .saved false => "discard"
    | .settled => "settled", state.composer⟩

structure Step where
  action : String
  expected : Observation
  deriving Lean.ToJson

structure Trace where
  mode : String
  accepted : Bool
  restore : Bool
  steps : List Step
  deriving Lean.ToJson

def recoveryTrace (saved accepted restore edit : Bool) : Trace := Id.run do
  let initial : State := ⟨if saved then .saved true else .sending, emptyDraft⟩
  let lost := loseReply initial
  let mut steps := [Step.mk "lost" (observe lost)]
  let current := if edit then { lost with composer := edited } else lost
  if edit then steps := steps ++ [⟨"edit", observe current⟩]
  let chosen := if saved then { current with phase := .saved restore } else current
  if saved then steps := steps ++ [⟨"choose", observe chosen⟩]
  steps := steps ++ [⟨"restart", observe chosen⟩, ⟨"wrongScope", observe chosen⟩]
  if saved then steps := steps ++ [⟨"wrongEpoch", observe chosen⟩, ⟨"wrongReceipt", observe chosen⟩,
    ⟨"rejected", observe chosen⟩]
  let result := if saved then savedReply chosen else localReply chosen accepted restore
  steps := steps ++ [⟨"reply", observe result⟩]
  if saved then steps := steps ++ [⟨"reply", observe (savedReply result)⟩]
  else steps := steps ++ [⟨"lost", observe (loseReply result)⟩]
  return ⟨if saved then "saved" else "local", accepted, restore, steps⟩

end Recovery

def main : IO Unit := do
  for restore in [false, true] do
    for edit in [false, true] do
      for accepted in [false, true] do
        IO.println (Lean.toJson (Recovery.recoveryTrace false accepted restore edit)).compress
      IO.println (Lean.toJson (Recovery.recoveryTrace true true restore edit)).compress
