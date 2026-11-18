import Lean

set_option warningAsError true

namespace HistoryPaging

inductive Entry where
  | work | user | agent | queued | steer | promoted | notification
  deriving DecidableEq, Lean.ToJson

inductive Cursor where
  | advance | same | missing
  deriving DecidableEq, Lean.ToJson

structure Page where
  rows : List Entry
  more : Bool := true
  cursor : Cursor := .advance
  deriving Lean.ToJson

inductive Decision where
  | continueReading | stop | retry
  deriving DecidableEq, Lean.ToJson

def boundary : Entry → Bool
  | .user | .agent | .queued | .notification => true
  | .work | .steer | .promoted => false

def step (p : Page) : Decision :=
  if !p.more || p.rows.any boundary then .stop
  else if p.cursor = .advance then .continueReading else .retry

-- Number of pages committed by one manual read and its next action. An empty
-- suffix means the server has yet to answer the outstanding request.
def read : List Page → Nat × Decision
  | [] => (0, .continueReading)
  | p :: tail =>
    match step p with
    | .stop => (1, .stop)
    | .retry => (0, .retry)
    | .continueReading => let (n, result) := read tail; (n + 1, result)

theorem cross_work (prefixPages tail : List Page)
    (valid : ∀ p ∈ prefixPages, p.more = true ∧ p.rows.any boundary = false ∧ p.cursor = .advance) :
    read (prefixPages ++ tail) = ((read tail).1 + prefixPages.length, (read tail).2) := by
  induction prefixPages with
  | nil => simp
  | cons p ps ih =>
    obtain ⟨more, noBoundary, advances⟩ := valid p (by simp)
    have rest := ih (fun q h => valid q (by simp [h]))
    simp [read, step, more, noBoundary, advances, rest, Nat.add_assoc]

theorem stops_at_first_boundary_or_end (prefixPages : List Page) (p : Page) (tail : List Page)
    (valid : ∀ q ∈ prefixPages, q.more = true ∧ q.rows.any boundary = false ∧ q.cursor = .advance)
    (found : p.rows.any boundary = true ∨ p.more = false) :
    read (prefixPages ++ p :: tail) = (prefixPages.length + 1, .stop) := by
  rw [cross_work _ _ valid]
  rcases found with h | h <;> simp [read, step, h, Nat.add_comm]

theorem stalled_page_is_retryable (prefixPages : List Page) (p : Page) (tail : List Page)
    (valid : ∀ q ∈ prefixPages, q.more = true ∧ q.rows.any boundary = false ∧ q.cursor = .advance)
    (more : p.more = true) (noBoundary : p.rows.any boundary = false) (stalled : p.cursor ≠ .advance) :
    read (prefixPages ++ p :: tail) = (prefixPages.length, .retry) := by
  rw [cross_work _ _ valid]
  simp [read, step, more, noBoundary, stalled]

structure Trace where
  pages : List Page
  committed : Nat
  decision : Decision
  deriving Lean.ToJson

def emit (pages : List Page) : IO Unit := do
  let (committed, decision) := read pages
  IO.println (Lean.toJson ({ pages, committed, decision } : Trace)).compress

end HistoryPaging

open HistoryPaging in
def main : IO Unit := do
  let work : Page := ⟨[.work], true, .advance⟩
  let prompt : Page := ⟨[.user], true, .advance⟩
  for rows in [[], [.work], [.user], [.work, .agent], [.queued, .work], [.steer],
      [.promoted], [.notification], [.steer, .notification, .work]] do
    for more in [false, true] do
      for cursor in [Cursor.advance, .same, .missing] do
        emit [work, ⟨rows, more, cursor⟩, work, prompt]
  -- The reported conversation has eight consecutive 75-record pages without a boundary.
  emit (List.replicate 8 ⟨List.replicate 75 .work, true, .advance⟩ ++ [prompt, work])
