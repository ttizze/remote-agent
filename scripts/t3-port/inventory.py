#!/usr/bin/env python3
"""Inventory the fixed T3 source, preserving reviewed per-file port records."""
import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

COMMIT = '4ee6bfd50ef4a089440d5c3662db2298da9cc50e'
ROOTS = ('apps/server/src/orchestration-v2', 'apps/server/src/project',
         'packages/client-runtime', 'packages/contracts')
DOCS = Path(__file__).resolve().parents[2] / 'docs/t3-port'


def category(path):
    name = path.name
    if name.endswith(('.test.ts', '.test.tsx', '.spec.ts', '.spec.tsx')):
        return 'test'
    if '/testkit/' in path.as_posix() or '.testkit.' in name or 'TestFixtures' in name:
        return 'testkit'
    if name.endswith(('.ndjson', '.json')):
        return 'fixture'
    return 'source'


def exclusion(path):
    s = path.as_posix()
    if '/legacy/' in s or path.name == 'Orchestrator.migration.test.ts':
        return '製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。'
    if '/fixtures/' in s and re.search(r'(?:/fixtures/(?:grok|opencode|pi|cursor)[^/]*|/(?:grok|opencode|pi|cursor|registry)_transcript|/(?:grok|opencode|pi|cursor)_output)', s):
        return 'Codex/Claude 以外の provider 専用 replay fixture。この実装では対象外。'
    if '/orchestration-v2/' in s and re.search(r'(Acp|Antigravity|Cursor|Devin|Grok|OpenCode|Pi(?:Adapter|Rpc|Orchestrator)|piT3)', path.name):
        return 'この実装の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。'
    if '/client-runtime/' in s:
        if any(x in s for x in ['/relay/', '/authorization/']):
            return 'T3 Connect の認証・relay は作らない。iroh/QR の境界は別途記録。'
        if path.name == 'remotePerformance.bench.ts':
            return 'Web/relay の benchmark。Rust の意味論テストには該当しない。'
    if '/contracts/' in s:
        if re.search(r'^(auth|environmentHttp|relay|relayClient|remoteAccess)', path.name):
            return 'T3 Connect の認証・HTTP・relay の契約。iroh/QR の境界は別途記録。'
    return None


def scan(ref):
    records = []
    for root in ROOTS:
        for file in sorted((ref / root).rglob('*')):
            if not file.is_file() or file.suffix not in ('.ts', '.tsx', '.ndjson', '.json'):
                continue
            relative = file.relative_to(ref)
            data = file.read_bytes()
            text = data.decode('utf8')
            kind = category(relative)
            why = exclusion(relative)
            symbols = []
            if kind == 'source':
                for match in re.finditer(r'(?m)^\s*(?:export\s+)?(?:async\s+)?(?:function\s+([A-Za-z_$][\w$]*)|const\s+([A-Za-z_$][\w$]*)\s*(?::[^=\n]+)?\s*=)', text):
                    symbols.append({'name': match[1] or match[2], 'line': text.count('\n', 0, match.start()) + 1,
                                    'status': '未照合'})
            cases = []
            if kind == 'test':
                for match in re.finditer(r'\b(?:it|test)(?:\.(?:effect|scoped|live|skip|todo))?\s*\(\s*(["\x27`])([^\n]+?)\1', text):
                    cases.append({'name': match[2], 'line': text.count('\n', 0, match.start()) + 1, 'status': '未移植'})
            records.append({'source': str(relative), 'sha256': hashlib.sha256(data).hexdigest(),
                            'lines': len(text.splitlines()), 'kind': kind,
                            'status': '対象外' if why else '未翻訳',
                            'reason': why, 'tests': [], 'symbols': symbols, 'cases': cases,
                            'reviewed_ranges': []})
    return records


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--reference', type=Path, default=Path('/tmp/t3code-ref'))
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    actual_commit = subprocess.check_output(['git', '-C', str(args.reference), 'rev-parse', 'HEAD'], text=True).strip()
    assert actual_commit == COMMIT, f'Wrong T3 commit: {actual_commit}'
    inventory = scan(args.reference)
    path = DOCS / 'PORT_MAP.json'
    previous = json.loads(path.read_text()) if path.exists() else {'files': []}
    old = {row['source']: row for row in previous['files']}
    for index, row in enumerate(inventory):
        if row['source'] in old:
            assert old[row['source']]['sha256'] == row['sha256'], f"Source changed: {row['source']}"
            previous_row = old[row['source']]
            # A previous generated row may still carry a boundary that was
            # removed from the current scope. Preserve reviewed records, but
            # refresh unreviewed exclusions from the current classifier.
            if previous_row['status'] == '対象外' and not previous_row['reviewed_ranges']:
                previous_row = {**previous_row, 'status': row['status'], 'reason': row['reason']}
            inventory[index] = previous_row
    if args.check:
        assert set(old) == {row['source'] for row in inventory}, 'Inventory contains missing or extra source files'
        assert previous['commit'] == COMMIT
        print(f'Fixed source inventory: {len(inventory)} files, hashes match.')
        return
    path.write_text(json.dumps({'commit': COMMIT, 'files': inventory}, ensure_ascii=False, indent=2) + '\n')
    document = (DOCS / 'PORT_MAP.md').read_text()
    intro, generated = document.split('<!-- generated inventory -->', 1)
    if '<!-- end generated inventory -->' in generated:
        notes = generated.split('<!-- end generated inventory -->', 1)[1]
    else:
        notes = '\n## 新設計の挙動テスト対応' + generated.split('\n## 新設計の挙動テスト対応', 1)[1]
    out = [intro, '<!-- generated inventory -->\n', '\n## ファイル対応表\n\n各ファイルの状態と対象外の理由を示す。新設計での対応先は「新設計の挙動テスト対応」以降に記録する。\n']
    for root in ROOTS:
        rows = [r for r in inventory if r['source'].startswith(root + '/')]
        production = [r for r in rows if r['kind'] == 'source']
        tests = [r for r in rows if r['kind'] == 'test']
        out += [f'\n### `{root}`\n\n本体 {len(production)} ファイル / {sum(r["lines"] for r in production):,} 行。テスト {len(tests)} ファイル / {sum(r["lines"] for r in tests):,} 行。testkit/fixture は別行で全件追跡する。\n',
                '\n| T3 のファイル（行数） | 移植した T3 テスト | 状態・理由 |\n|---|---|---|\n']
        for r in rows:
            tests = '<br>'.join(r['tests']) or '—'
            out.append(f'| `{r["source"]}` ({r["lines"]}) | {tests} | {r["status"]}' + (f'：{r["reason"]}' if r['reason'] else '') + ' |\n')
    (DOCS / 'PORT_MAP.md').write_text(''.join(out) + '\n<!-- end generated inventory -->' + notes)


if __name__ == '__main__':
    main()
