/// Guards a native buffer against acknowledgements for older edits.
struct DraftRevision {
    private(set) var pending: UInt64?
    var base = ""
    private var revision: UInt64 = 0
    mutating func edit(_ text: String) -> (revision: UInt64, base: String) {
        let previous = base
        base = text
        revision += 1
        pending = revision
        return (revision, previous)
    }

    mutating func acknowledge(_ completed: UInt64) -> Bool {
        guard pending == completed else { return false }
        pending = nil
        return true
    }

    mutating func reset() {
        revision += 1; pending = nil; base = ""
    }
}
