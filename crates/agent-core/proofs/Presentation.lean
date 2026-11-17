import Lean

namespace Presentation

-- Indices refer to the original tab order, not the host's opaque surface IDs.
def afterClose (count active closed : Nat) : Option Nat :=
  if active != closed || closed ≥ count then some active
  else if closed + 1 < count then some (closed + 1)
  else if closed > 0 then some (closed - 1)
  else none

theorem surviving_active (count active closed : Nat) (h : active ≠ closed) :
    afterClose count active closed = some active := by simp [afterClose, h]

theorem closing_active_selects_survivor (count active : Nat) (valid : active < count)
    (others : 1 < count) :
    ∃ next, afterClose count active active = some next ∧ next < count ∧ next ≠ active := by
  by_cases right : active + 1 < count
  · exact ⟨active + 1, by simp [afterClose, right, Nat.not_le.mpr valid], right, by omega⟩
  · have left : 0 < active := by omega
    exact ⟨active - 1, by simp [afterClose, right, left, Nat.not_le.mpr valid], by omega, by omega⟩

def advance (answers : List Bool) (saved : Nat) : Nat × Bool :=
  let index := min saved (answers.length - 1)
  if index + 1 < answers.length then
    (if answers[index]? == some true then index + 1 else index, false)
  else (index, answers.all id)

theorem submission_requires_all (answers : List Bool) (saved : Nat)
    (sent : (advance answers saved).2 = true) : answers.all id = true := by
  dsimp only [advance] at sent
  split at sent <;> simp_all

theorem advance_stays_in_bounds (answers : List Bool) (saved : Nat) (h : 0 < answers.length) :
    (advance answers saved).1 < answers.length := by
  have bounded : min saved (answers.length - 1) < answers.length := by omega
  by_cases room : min saved (answers.length - 1) + 1 < answers.length
  · by_cases answered : answers[min saved (answers.length - 1)]? == some true
    · simp [advance, room, answered]
    · simpa [advance, room, answered] using bounded
  · simpa [advance, room] using bounded

-- Positive view dimensions are expressed in half-points so ties are exact.
def dimension (halfPoints lower upper : Nat) := min upper (max lower ((halfPoints + 1) / 2))

theorem dimension_in_range (halfPoints lower upper : Nat) (h : lower ≤ upper) :
    lower ≤ dimension halfPoints lower upper ∧ dimension halfPoints lower upper ≤ upper := by
  unfold dimension
  omega

def resize (width height currentWidth currentHeight : Nat) : Option (Nat × Nat) :=
  let target := (dimension width 320 7680, dimension height 240 4320)
  if width = 0 ∨ height = 0 ∨ target = (currentWidth, currentHeight) then none else some target

theorem applied_resize_is_stable (width height oldWidth oldHeight nextWidth nextHeight : Nat)
    (sent : resize width height oldWidth oldHeight = some (nextWidth, nextHeight)) :
    resize width height nextWidth nextHeight = none := by
  dsimp only [resize] at *
  split at sent
  · contradiction
  · simp only [Option.some.injEq, Prod.mk.injEq] at sent
    simp [sent.1, sent.2]

def loadHistory (more busy oldest latest following : Bool) : Bool :=
  more && !busy && oldest && (!following || latest)

theorem loading_excludes_duplicate (more oldest latest following : Bool) :
    loadHistory more true oldest latest following = false := by simp [loadHistory]

theorem following_requires_both_edges (more oldest latest : Bool)
    (load : loadHistory more false oldest latest true = true) : oldest = true ∧ latest = true := by
  cases more <;> cases oldest <;> cases latest <;> simp_all [loadHistory]

def enabledTools (connected directory : Bool) : List String :=
  ["browser"] ++ (if connected && directory then ["terminal", "files", "diff", "sideChat"] else []) ++ ["agents"]

theorem restricted_tools_require_both (connected directory : Bool) (tool : String)
    (enabled : tool ∈ enabledTools connected directory) (remote : tool ≠ "browser" ∧ tool ≠ "agents") :
    connected = true ∧ directory = true := by
  cases connected <;> cases directory <;> simp_all [enabledTools]

structure ToolCase where
  connected : Bool
  directory : Bool
  enabled : List String
  deriving Lean.ToJson

structure CloseCase where
  count : Nat
  active : Nat
  closed : Nat
  selected : Option Nat
  deriving Lean.ToJson

structure AdvanceCase where
  answers : List Bool
  saved : Nat
  index : Nat
  sent : Bool
  deriving Lean.ToJson

structure ResizeCase where
  width : Nat
  height : Nat
  currentWidth : Nat
  currentHeight : Nat
  nextWidth : Option Nat
  nextHeight : Option Nat
  deriving Lean.ToJson

structure HistoryCase where
  more : Bool
  busy : Bool
  oldest : Bool
  latest : Bool
  following : Bool
  load : Bool
  deriving Lean.ToJson

def emit [Lean.ToJson α] (kind : String) (row : α) : IO Unit := do
  IO.println (Lean.Json.mkObj [("kind", Lean.toJson kind), ("case", Lean.toJson row)]).compress

def bools := [false, true]

end Presentation

open Presentation

def main : IO Unit := do
  for connected in bools do
    for directory in bools do
      emit "tools" ({ connected, directory, enabled := enabledTools connected directory } : ToolCase)
  for count in [1, 2, 3, 4] do
    for active in List.range count do
      for closed in List.range (count + 1) do
        emit "close" ({ count, active, closed, selected := afterClose count active closed } : CloseCase)
  for first in bools do
    for second in bools do
      for third in bools do
        for saved in [0, 1, 2, 3, 20] do
          let answers := [first, second, third]
          let (index, sent) := advance answers saved
          emit "advance" ({ answers, saved, index, sent } : AdvanceCase)
  for width in [0, 1, 639, 640, 641, 642, 15359, 15360, 16000] do
    for height in [0, 1, 479, 480, 481, 482, 8639, 8640, 9000] do
      let targetWidth := dimension width 320 7680
      let targetHeight := dimension height 240 4320
      for currentWidth in [targetWidth, targetWidth + 1] do
        for currentHeight in [targetHeight, targetHeight + 1] do
          let next := resize width height currentWidth currentHeight
          emit "resize" ({ width, height, currentWidth, currentHeight, nextWidth := next.map Prod.fst, nextHeight := next.map Prod.snd } : ResizeCase)
  for more in bools do
    for busy in bools do
      for oldest in bools do
        for latest in bools do
          for following in bools do
            emit "history" ({ more, busy, oldest, latest, following, load := loadHistory more busy oldest latest following } : HistoryCase)
