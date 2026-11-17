import Lean

set_option warningAsError true

namespace Workspace

structure File where
  revision : String
  text : String
  deriving DecidableEq, Lean.ToJson

structure State where
  opened : Option File
  cached : Option File
  draft : Option File

def switch (state : State) : State := ⟨state.cached, state.opened, state.draft⟩

def edit (state : State) (text : String) : State :=
  { state with draft := (state.draft <|> state.opened).map fun base => { base with text } }

def remaining (submitted saved : File) (draft : Option File) : Option File := do
  let current ← draft
  if current = submitted then none
  else if current.revision = submitted.revision then some { current with revision := saved.revision }
  else some current

def complete (state : State) (submitted saved : File) : State :=
  ⟨state.opened.map (fun _ => saved), state.cached.map (fun _ => saved),
   remaining submitted saved state.draft⟩

theorem saved_submission_clears (submitted saved : File) :
    remaining submitted saved (some submitted) = none := by simp [remaining]

theorem later_edit_survives (submitted saved : File) (text : String) (later : text ≠ submitted.text) :
    remaining submitted saved (some ⟨submitted.revision, text⟩) = some ⟨saved.revision, text⟩ := by
  cases submitted
  simp_all [remaining]

theorem complete_commutes_with_switch (state : State) (submitted saved : File) :
    complete (switch state) submitted saved = switch (complete state submitted saved) := rfl

structure Observation where
  opened : Option File
  nextSave : Option File
  editorText : String
  canSave : Bool
  deriving Lean.ToJson

def observe (state : State) : Observation :=
  let text := ((state.draft <|> state.opened).map File.text).getD ""
  ⟨state.opened, state.draft, text, text != (state.opened.map File.text).getD ""⟩

inductive Route where
  | stay | away | roundTrip
  deriving DecidableEq, Lean.ToJson

structure Trace where
  route : Route
  later : Option String
  captured : File
  beforeReply : Observation
  afterReply : Observation
  back : Observation
  deriving Lean.ToJson

def trace (route : Route) (later : Option String) : Trace := Id.run do
  let base : File := ⟨"old-revision", "OLD"⟩
  let initial := edit ⟨some base, none, none⟩ "NEW"
  let submitted := initial.draft.getD base
  let edited := later.elim initial (edit initial)
  let waiting := match route with
    | .stay => edited
    | .away => switch edited
    | .roundTrip => switch (switch edited)
  let doneState := complete waiting submitted ⟨"saved-revision", submitted.text⟩
  return ⟨route, later, submitted, observe waiting, observe doneState,
    observe (if route = .away then switch doneState else doneState)⟩

end Workspace

def main : IO Unit := do
  let stdout ← IO.getStdout
  for route in [Workspace.Route.stay, .away, .roundTrip] do
    for later in [none, some "NEW", some "NEXT", some "OLD", some ""] do
      stdout.putStrLn (Lean.toJson (Workspace.trace route later)).compress
