-- 一行だけ持つ。保存領域の識別子、連番、削除記録を消し終えた連番。
CREATE TABLE store (
    id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    removals_pruned_through INTEGER NOT NULL,
    updating INTEGER NOT NULL CHECK (updating IN (0, 1))
) STRICT;

-- 同期する表
CREATE TABLE threads (
    key INTEGER PRIMARY KEY,        -- DB の中だけで使う行のキー
    provider TEXT,                  -- 会話の識別子。Bex で作った会話は provider を持たない
    id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    loaded INTEGER NOT NULL DEFAULT 1 CHECK (loaded IN (-1, 0, 1)), -- -1: unlisted parent, 0: catalog only, 1: imported
    parent INTEGER,                 -- サブエージェントの親の行のキー
    cwd TEXT,                       -- 取り込んだ会話では不明なことがある
    updated_at INTEGER NOT NULL,    -- 一覧の順
    history_seq INTEGER NOT NULL,   -- 最後に履歴を置き換えた連番
    body BLOB NOT NULL              -- 名前、今の provider、モデル選択、既読、キューの保留、一覧に出す要約
) STRICT;
CREATE UNIQUE INDEX threads_identity ON threads (ifnull(provider, ''), id);
CREATE INDEX threads_changed ON threads (seq);
CREATE INDEX threads_children ON threads (parent) WHERE parent IS NOT NULL;

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    seq INTEGER NOT NULL,
    body BLOB NOT NULL              -- 名前、フォルダ
) STRICT;
CREATE INDEX projects_changed ON projects (seq);

CREATE TABLE inputs (
    id TEXT PRIMARY KEY,            -- 操作の識別子を兼ね、配送後は発言の項目の識別子になる
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    position INTEGER NOT NULL,      -- キューの順
    state TEXT NOT NULL,            -- preparing | queued | delivering | unknown | failed
    body BLOB NOT NULL              -- 内容、配送の種類、選んだモデル、状態ごとの値
) STRICT;
CREATE INDEX inputs_queue ON inputs (thread, position);
CREATE INDEX inputs_in_flight ON inputs (state) WHERE state IN ('preparing', 'delivering', 'unknown');

CREATE TABLE turns (
    id TEXT NOT NULL,
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,       -- 使い回さない
    state TEXT NOT NULL,            -- running | waiting | completed | interrupted | failed | rolled_back
    body BLOB NOT NULL,             -- 時刻、状態ごとの値(待機中の作業、再試行の時刻、エラー)
    PRIMARY KEY (thread, id)
) STRICT;
CREATE UNIQUE INDEX turns_order ON turns (thread, ordinal);
CREATE INDEX turns_in_flight ON turns (state) WHERE state IN ('running', 'waiting');

CREATE TABLE items (
    id TEXT NOT NULL,
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    turn_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    position INTEGER NOT NULL,      -- 会話の中での順
    status TEXT NOT NULL,
    body BLOB NOT NULL,             -- 種類ごとの内容。本文を含む
    PRIMARY KEY (thread, id),
    FOREIGN KEY (thread, turn_id) REFERENCES turns (thread, id) ON DELETE CASCADE
) STRICT;
CREATE UNIQUE INDEX items_order ON items (thread, position);
CREATE INDEX items_unfinished ON items (thread, turn_id) WHERE status = 'running';
CREATE INDEX items_changed ON items (thread, seq);
CREATE INDEX items_turn ON items (thread, turn_id, position);

CREATE TABLE requests (
    id TEXT PRIMARY KEY,
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    state TEXT NOT NULL,            -- awaiting | sending | unknown
    body BLOB NOT NULL              -- 対象のターンと項目、承認か質問の内容
) STRICT;
CREATE INDEX requests_thread ON requests (thread);
CREATE INDEX requests_in_flight ON requests (state) WHERE state = 'sending';

-- 同期のための表
CREATE TABLE removals (
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,             -- input | turn | item | request | project
    id TEXT NOT NULL,
    thread INTEGER,                 -- 会話の範囲の行なら、その会話の行のキー。一覧だけの行では空
    at INTEGER NOT NULL             -- 保持期間の判定に使う
) STRICT;
CREATE UNIQUE INDEX removals_identity ON removals (seq, kind, id, ifnull(thread, 0));
CREATE INDEX removals_changed ON removals (seq);
CREATE INDEX removals_expiry ON removals (at);
CREATE INDEX removals_thread ON removals (thread, seq) WHERE thread IS NOT NULL;

CREATE TABLE receipts (
    operation_id TEXT PRIMARY KEY,
    provider TEXT,                  -- 対象の会話の識別子。会話を消した後も残すので、行のキーでは持たない
    thread_id TEXT NOT NULL,
    seq INTEGER NOT NULL,           -- 効果を書いたトランザクションの連番
    outcome BLOB NOT NULL           -- 受付か、理由付きの拒否
) STRICT, WITHOUT ROWID;

-- Host だけが読む表

-- 会話に関わった provider のセッション。今の対応も、作り直す前のものも、取り込み元も含む。
CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    native_id TEXT NOT NULL,        -- provider のセッションの識別子
    context BLOB,                   -- 初回の入力に添える履歴。native injection 非対応時だけ保持
    options BLOB                    -- この native 資源で確定したモデルと選択肢
) STRICT;
CREATE UNIQUE INDEX sources_native ON sources (provider, native_id);

-- provider ごとに、今使うセッション。
CREATE TABLE bindings (
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    source_id INTEGER NOT NULL REFERENCES sources (id),
    covered INTEGER,                -- そのセッションが知っている最後のターンの順序番号
    state TEXT NOT NULL,            -- ready | aligning | fork
    body BLOB NOT NULL,             -- 分岐で保持する native のターン
    PRIMARY KEY (thread, provider)
) STRICT;

-- ターンの試行と、それを実行した provider のセッション、provider のターン。
CREATE TABLE executions (
    thread INTEGER NOT NULL REFERENCES threads (key) ON DELETE CASCADE,
    turn_id TEXT NOT NULL,
    attempt TEXT NOT NULL,          -- Host が割り当てる試行の UUID。再起動後も使い回さない
    source_id INTEGER NOT NULL REFERENCES sources (id),
    native_turn_id TEXT,
    submitted INTEGER NOT NULL DEFAULT 0 CHECK (submitted IN (0, 1)),
    callbacks INTEGER NOT NULL DEFAULT 1 CHECK (callbacks IN (0, 1)),
    native_position INTEGER,        -- 外部履歴で識別子が再利用された場合の位置
    started_seq INTEGER NOT NULL,
    current INTEGER NOT NULL CHECK (current IN (0, 1)),
    process TEXT NOT NULL,          -- この試行を所有する Host プロセスの UUID
    PRIMARY KEY (thread, turn_id, attempt)
) STRICT, WITHOUT ROWID;
CREATE UNIQUE INDEX executions_current ON executions (thread, turn_id) WHERE current = 1;
CREATE UNIQUE INDEX executions_attempt ON executions (attempt);
CREATE INDEX executions_native ON executions (source_id, native_turn_id) WHERE native_turn_id IS NOT NULL;

-- 項目と provider の項目の対応。試行の中でだけ一意である。
CREATE TABLE item_natives (
    thread INTEGER NOT NULL,
    turn_id TEXT NOT NULL,
    attempt TEXT NOT NULL,
    native_id TEXT NOT NULL,        -- provider の項目の識別子
    item_id TEXT NOT NULL,
    PRIMARY KEY (thread, turn_id, attempt, native_id),
    FOREIGN KEY (thread, item_id) REFERENCES items (thread, id) ON DELETE CASCADE,
    FOREIGN KEY (thread, turn_id, attempt) REFERENCES executions (thread, turn_id, attempt) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- 再実行しても同じ効果しか起こさない副作用。必要とした変更と同じトランザクションで書く。
CREATE TABLE jobs (
    position INTEGER PRIMARY KEY AUTOINCREMENT, -- 列の中の追加順。完了後も番号を使い回さない
    id TEXT NOT NULL UNIQUE,        -- 再試行でも変えない仕事の識別子
    kind TEXT NOT NULL,             -- projects_changed | catalog | continue | import
    lane TEXT NOT NULL,             -- 順序を共有する仕事の列
    state TEXT NOT NULL CHECK (state IN ('pending', 'running')),
    run_after INTEGER NOT NULL,     -- 次に実行してよい時刻
    attempts INTEGER NOT NULL,
    last_error TEXT,
    seq INTEGER NOT NULL DEFAULT 0, -- catalog progress shares the list cursor
    body BLOB NOT NULL              -- 対象の SessionRef、要求・試行、接続の世代など、種類ごとの値
) STRICT;
CREATE INDEX jobs_due ON jobs (run_after, position) WHERE state = 'pending';
CREATE INDEX jobs_lane ON jobs (lane, position);

CREATE TABLE workspace_scripts (root TEXT PRIMARY KEY, body BLOB NOT NULL) STRICT;
