import Lean

set_option warningAsError true
namespace Composer
open Lean

def keep (retained provider token : Bool) := retained || (provider && token)
def refresh (loading : Option Bool) (emptyFilter changed : Bool) :=
  match loading with
  | none => true
  | some active => !active && emptyFilter && changed
theorem in_flight_is_not_restarted (emptyFilter changed : Bool) :
    refresh (some true) emptyFilter changed = false := rfl
def capture (image duplicate : Bool) := image && !duplicate
def fits (used width : Nat) := decide (used + width ≤ 256)
theorem full_query_excludes_next (width : Nat) (positive : 0 < width) :
    fits 256 width = false := by simp [fits] <;> omega
def save (content busy : Bool) (count : Nat) : String :=
  if !content then "unchanged" else if busy || count ≥ 20 then "error" else "moved"
def prepare (present busy : Bool) : String :=
  if present then "unchanged" else if busy then "error" else "appended"

theorem retained_survives (provider token : Bool) : keep true provider token = true := rfl
theorem duplicate_never_appends (image : Bool) : capture image true = false := by cases image <;> rfl
theorem full_stash_never_moves (content busy : Bool) (count : Nat) (full : count ≥ 20) :
    save content busy count ≠ "moved" := by cases content <;> simp [save, full]
theorem prepared_is_stable (busy : Bool) : prepare true busy = "unchanged" := rfl

def uniqueDocument : List Nat → Option Nat
  | [document] => some document
  | _ => none
theorem ambiguous_never_navigates (a b : Nat) (rest : List Nat) :
    uniqueDocument (a :: b :: rest) = none := rfl

def ecmaSpace (n : Nat) : Bool :=
  decide (9 ≤ n ∧ n ≤ 13) || decide (0x2000 ≤ n ∧ n ≤ 0x200A) ||
    [32, 160, 0x1680, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000, 0xFEFF].contains n

def collapseRun (n : Nat) := if ecmaSpace n then " " else String.singleton (Char.ofNat n) ++ String.singleton (Char.ofNat n)
theorem space_run_is_one (n : Nat) (h : ecmaSpace n = true) : collapseRun n = " " := by simp [collapseRun, h]

def slot (args : List String) (index : Nat) := args[index]?.getD ("{" ++ toString index ++ "}")
theorem inserted_verbatim (value : String) : slot [value] 0 = value := rfl

-- Integer half-LSB inputs are exactly representable over the emitted range.
def pcm (halfUnits : Int) : Int :=
  let rounded := if halfUnits < 0 then - ((-halfUnits + 1) / 2) else (halfUnits + 1) / 2
  max (-32768) (min 32767 rounded)
theorem pcm_bounds (units : Int) : -32768 ≤ pcm units ∧ pcm units ≤ 32767 := by
  dsimp only [pcm]
  omega

def emit (kind : String) (args : Array Json) (expected : Json) : IO Unit :=
  IO.println (Json.mkObj [("kind", toJson kind), ("args", .arr args), ("expected", expected)]).compress

def bools := [false, true]

def drafts : IO Unit := do
  for first in bools do
    for second in bools do
      let pair := #[toJson first, toJson second]
      for loading in [none, some false, some true] do
        emit "refresh" #[toJson loading, toJson first, toJson second] (toJson (refresh loading first second))
      emit "capture" pair (toJson (capture first second))
      emit "template" pair (toJson (prepare first second))
      let found := (if first then [0] else []) ++ (if second then [1] else [])
      emit "navigate" pair (toJson (uniqueDocument found))
      emit "drop" pair (toJson (if second then none else some (if first then ["New"] else ["Old", "New"])))
      for third in bools do
        emit "invocation" (pair.push (toJson third)) (toJson (keep first second third))
      for count in [0, 19, 20, 21] do
        emit "stash" (pair.push (toJson count)) (toJson (save first second count))
  for count in [0, 253, 254, 255, 256, 257] do
    let ascii := String.ofList (List.replicate count 'x')
    let result := String.ofList (List.replicate (min count 256) 'x') ++
      (if fits count 2 then "😀" else "") ++ (if fits count 3 then "z" else "")
    emit "path" #[toJson (ascii ++ "😀z")] (toJson result)
  for marker in ["`", "~"] do
    for width in [2, 3, 4] do
      for (info, language) in [("js", "javascript"), ("txt", ""), ("other", "other")] do
        let fence := String.join (List.replicate width marker)
        emit "fence" #[toJson (fence ++ info ++ "\nbody\n" ++ fence)] (toJson (if width ≥ 3 then [language] else []))
  for draft in ["", "existing"] do
    for lines in [[""], ["one"], ["日", "", "two", ""]] do
      let quoted := String.intercalate "\n" (lines.map ("> " ++ ·)) ++ "\n\n"
      emit "quote" #[toJson draft, toJson (String.intercalate "\n" lines)]
        (toJson ((if draft.isEmpty then "" else draft ++ "\n\n") ++ quoted))

def copyCases : IO Unit := do
  for english in bools do
    for value in ["", "{0}", "設定\n{x}"] do
      let fragment := slot [value] 0
      emit "format" #[toJson english, toJson "残り {0}%", toJson value]
        (toJson (if english then fragment ++ "% remaining" else "残り " ++ fragment ++ "%"))
      emit "format" #[toJson english, toJson "{1}:{0}:{0}:{x}:{", toJson value]
        (toJson (slot [value] 1 ++ ":" ++ fragment ++ ":" ++ fragment ++ ":{x}:{"))
  let boundaries := [9, 13, 32, 0x85, 160, 0x1680, 0x2000, 0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000, 0xFEFF]
  let points := boundaries.flatMap fun base => [base - 1, base, base + 1]
  let codes : List Nat := points ++ (List.range 11).map (0x2000 + ·)
  for code in codes do
    emit "space" #[toJson code] (.arr #[toJson (ecmaSpace code), toJson (collapseRun code)])
  let policy ← IO.FS.readFile "docs/PRIVACY.md"
  emit "privacy" #[] (toJson policy)
  for samples in [([] : List Int), [-131072, -65536, -65535, -3, -2, -1, 0, 1, 2, 3, 65533, 65534, 65535, 65536, 131072]] do
    let bytes := samples.flatMap fun units => [pcm units % 256, (pcm units / 256) % 256]
    emit "pcm" #[toJson samples] (toJson (if samples.isEmpty then none else some bytes))

end Composer

def main : IO Unit := do
  Composer.drafts
  Composer.copyCases
