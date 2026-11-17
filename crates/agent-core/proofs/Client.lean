import Lean

set_option warningAsError true

namespace Client

inductive OwnerAction where
  | releaseBorrow | replaceHost | removeHost
  deriving Lean.ToJson

inductive Slot where
  | original | replacement | absent
  deriving DecidableEq, Lean.ToJson

def ownerSlot : OwnerAction → Slot
  | .releaseBorrow => .original
  | .replaceHost => .replacement
  | .removeHost => .absent

structure EditResult where
  applied : Bool
  text : String
  deriving DecidableEq, Lean.ToJson

def edit (slot : Slot) (storage capturedStorage : String) (view : Option Nat)
    (current : Nat) (before after : String) : EditResult :=
  if slot = .original ∧ storage = capturedStorage ∧ (view = none ∨ view = some current)
  then ⟨true, after⟩ else ⟨false, before⟩

variable (storage captured before after : String) (view : Option Nat) (current : Nat)

theorem retired_queue_preserves_draft (slot : Slot) (retired : slot ≠ .original) :
    edit slot storage captured view current before after = ⟨false, before⟩ := by
  simp [edit, retired]

theorem stale_view_preserves_draft (old : Nat) (moved : old ≠ current) :
    edit .original storage storage (some old) current before after = ⟨false, before⟩ := by
  simp [edit, moved]

theorem foreign_storage_preserves_draft (foreignStorage : storage ≠ captured) :
    edit .original storage captured view current before after = ⟨false, before⟩ := by
  simp [edit, foreignStorage]

theorem released_borrow_keeps_namespace_edit :
    edit (ownerSlot .releaseBorrow) storage storage none current before after = ⟨true, after⟩ := by
  simp [edit, ownerSlot]

structure Trace where
  action : OwnerAction
  navigate : Bool
  capturedStorage : String
  viewBound : Bool
  slot : Slot
  first : EditResult
  second : EditResult
  deriving Lean.ToJson

def trace (action : OwnerAction) (navigate : Bool) (captured : String) (viewBound : Bool) : Trace :=
  let slot := ownerSlot action
  let view := if viewBound then some 0 else none
  let first := edit slot "host" "host" (some 0) 0 "before" "first"
  let second := edit slot "host" captured view (if navigate then 1 else 0) first.text "second"
  ⟨action, navigate, captured, viewBound, slot, first, second⟩

end Client

def main : IO Unit := do
  for action in [Client.OwnerAction.releaseBorrow, .replaceHost, .removeHost] do
    for navigate in [false, true] do
      for captured in ["host", "other"] do
        for viewBound in [false, true] do
          IO.println (Lean.toJson (Client.trace action navigate captured viewBound)).compress
