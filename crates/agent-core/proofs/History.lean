import Lean

set_option warningAsError true

namespace History

structure Row where
  parent : Bool
  id : Nat
  version : Nat
  request : Bool := false
  affected : Bool := true
  deriving DecidableEq, Lean.ToJson

def key (row : Row) := (row.parent, row.id)

def visible (rollback : Bool) (row : Row) : Bool :=
  row.parent || !(rollback && row.affected)

def rollbackRows (rollback : Bool) (live : List Row) := live.filter (visible rollback)

def resolve (live cache : List Row) (incoming : Row) : Option Row :=
  match live.find? (fun row => key row == key incoming) with
  | some current => some current
  | none =>
    if incoming.parent then some incoming else
      match cache.find? (fun row => row.id == incoming.id) with
      | none => some incoming
      | some saved => if saved.request then some saved else none

def uniqueRows (seen : List (Bool × Nat)) : List Row → List Row
  | [] => []
  | row :: rest =>
    if key row ∈ seen then uniqueRows seen rest
    else row :: uniqueRows (key row :: seen) rest

def pageRows (rollback : Bool) (live cache older : List Row) :=
  uniqueRows [] ((older.filterMap (resolve live cache)).filter (visible rollback))

def merge (rollback : Bool) (initial cache older : List Row) : List Row × List Row :=
  let live := rollbackRows rollback initial
  let page := pageRows rollback live cache older
  let tailRows := live.filter (fun row => !(page.any (fun olderRow => key olderRow == key row)))
  let newItems := page.filter (fun row => !row.parent && !(cache.any (fun saved => saved.id == row.id)))
  (page ++ tailRows, cache ++ newItems)

theorem unique_subset {row : Row} (rows : List Row) (seen : List (Bool × Nat)) :
    row ∈ uniqueRows seen rows → row ∈ rows := by
  induction rows generalizing seen with
  | nil => simp [uniqueRows]
  | cons head rest ih =>
    simp only [uniqueRows]
    split
    · exact fun member => List.mem_cons_of_mem _ (ih _ member)
    · simp only [List.mem_cons]
      exact fun member => member.imp_right (ih _)

theorem rollback_never_revives (initial cache older : List Row) (row : Row)
    (member : row ∈ (merge true initial cache older).1) : visible true row = true := by
  simp only [merge, List.mem_append] at member
  rcases member with fromPage | fromLive
  · exact (List.mem_filter.mp (unique_subset _ [] fromPage)).2
  · exact (List.mem_filter.mp (List.mem_filter.mp fromLive).1).2

theorem live_body_wins (live cache : List Row) (incoming current : Row)
    (found : live.find? (fun row => key row == key incoming) = some current) :
    resolve live cache incoming = some current := by simp [resolve, found]

theorem unique_keys (rows : List Row) (seen : List (Bool × Nat)) :
    ((uniqueRows seen rows).map key).Nodup ∧
    ∀ k ∈ (uniqueRows seen rows).map key, k ∉ seen := by
  induction rows generalizing seen with
  | nil => simp [uniqueRows]
  | cons head rest ih =>
    simp only [uniqueRows]
    split
    · exact ih seen
    · rename_i fresh
      obtain ⟨nodup, disjoint⟩ := ih (key head :: seen)
      simp only [List.map_cons, List.nodup_cons, List.mem_cons]
      constructor
      · exact ⟨fun h => disjoint _ h (by simp), nodup⟩
      · intro k member
        rcases member with rfl | h
        · exact fresh
        · exact fun old => disjoint k h (List.mem_cons_of_mem _ old)

theorem inherited_survives_local_rollback (row : Row) (parent : row.parent = true) :
    merge true [] [] [row] = ([row], []) := by
  simp [merge, rollbackRows, pageRows, uniqueRows, resolve, visible, parent]

structure Case where
  rollback : Bool
  initial : List Row
  cache : List Row
  older : List Row
  expectedRows : List Row
  expectedItems : List Row
  deriving Lean.ToJson

def cases : List Case := Id.run do
  let mut result := []
  for rollback in [false, true] do
    for saved in [false, true] do
      for shown in (if saved then [false, true] else [false]) do
        for request in [false, true] do
          for affected in [false, true] do
            let localRow : Row := ⟨false, 1, 9, request, affected⟩
            let inherited := { localRow with parent := true, version := 8 }
            let tailRow : Row := ⟨false, 3, 7, false, false⟩
            let cache := (if saved then [localRow] else []) ++ [tailRow]
            let stale := { localRow with version := 1, request := false }
            let fresh : Row := ⟨false, 2, 2, false, true⟩
            for parentLive in [false, true] do
              let initial := (if parentLive then [inherited] else []) ++
                (if shown then [localRow] else []) ++ [tailRow]
              let page := [stale, { inherited with version := 1 }, fresh, { stale with version := 2 }]
              for older in [[], page, page.reverse] do
                let (rows, items) := merge rollback initial cache older
                result := { rollback, initial, cache, older, expectedRows := rows, expectedItems := items } :: result
  return result

end History

def main : IO Unit := do
  for testCase in History.cases do
    IO.println (Lean.toJson testCase).compress
