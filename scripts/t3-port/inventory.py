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


def snake(name):
    return re.sub(r'(?<!^)(?=[A-Z])', '_', name).replace('.', '_').replace('-', '_').lower()


def category(path):
    name = path.name
    if name.endswith(('.test.ts', '.test.tsx', '.spec.ts', '.spec.tsx')):
        return 'test'
    if '/testkit/' in path.as_posix() or '.testkit.' in name or 'TestFixtures' in name:
        return 'testkit'
    if name.endswith(('.ndjson', '.json')):
        return 'fixture'
    return 'source'


def destination(path):
    s = path.as_posix()
    name = path.stem.replace('.test', '').replace('.spec', '')
    name = name.split('.')[0]
    if '/orchestration-v2/testkit/' in s:
        sub = path.relative_to('apps/server/src/orchestration-v2/testkit')
        if '/fixtures/' in s and path.suffix == '.ndjson':
            return 'crates/provider-adapters/src/testkit/' + str(sub)
        return 'crates/orchestration/src/testkit/' + '/'.join(snake(part) for part in sub.with_suffix('').parts) + '.rs'
    if '/orchestration-v2/Adapters/' in s:
        return f'crates/provider-adapters/src/{snake(name)}.rs'
    if '/orchestration-v2/' in s:
        return f'crates/orchestration/src/{snake(name)}.rs'
    if '/project/' in s:
        return f'crates/host-daemon/src/project/{snake(name)}.rs'
    if 'packages/contracts/' in s:
        return f'crates/orchestration/src/contracts/{snake(name)}.rs'
    sub = path.relative_to('packages/client-runtime').as_posix()
    sub = sub.removeprefix('src/').removesuffix('.ts').removesuffix('.tsx')
    return 'crates/agent-core/src/' + '/'.join(snake(part) for part in sub.split('/')) + '.rs'


def exclusion(path):
    s = path.as_posix()
    if '/legacy/' in s or path.name == 'Orchestrator.migration.test.ts':
        return '製品は未公開。ユーザー指示により旧 V1 の互換性・移行を作らない。'
    if '/fixtures/' in s and re.search(r'(?:/fixtures/(?:grok|opencode|pi|cursor)[^/]*|/(?:grok|opencode|pi|cursor|registry)_transcript|/(?:grok|opencode|pi|cursor)_output)', s):
        return 'Codex/Claude 以外の provider 専用 replay fixture。Bex では対象外。'
    if '/orchestration-v2/' in s and re.search(r'(Acp|Antigravity|Cursor|Devin|Grok|OpenCode|Pi(?:Adapter|Rpc|Orchestrator)|piT3)', path.name):
        return 'Bex の provider は Codex と Claude。その他の provider の実装・専用試験は対象外。'
    if '/orchestration-v2/' in s and re.search(r'(PullRequest|pullRequestWatch|workflowScriptQuery)', path.name):
        return '今回の指示では M3（PR・workflow 周辺機能）を実装しない。'
    if '/client-runtime/' in s:
        if any(x in s for x in ['/device/', '/relay/', '/authorization/']):
            return 'T3 Connect の認証・relay、Web 用の仮想 device viewer は作らない。iroh/QR の境界は別途記録。'
        if any(x in s for x in ['pullRequest', 'PullRequest', '/vcs', '/gitActions', '/git.ts', '/usage.', '/sharedSettings', '/outdated', '/codexArtifactTemplates', '/projectFavicon', '/worktreeSetup']):
            return '今回の指示では M3 の Git/PR/usage/全設定・更新配布 UI は実装しない。会話に必要な契約・回復経路は除外しない。'
        if path.name == 'remotePerformance.bench.ts':
            return 'Web/relay の benchmark。Rust の意味論テストには該当しない。'
    if '/contracts/' in s:
        if re.search(r'^(acpRegistry|auth|browserImport|browserProfile|desktopAppActivation|desktopBootstrap|device|editor|environmentHttp|keybindings|preview|previewAutomation|relay|relayClient|remoteAccess|resourceTelemetry)', path.name):
            return 'T3 Connect/ブラウザ・Web 専用/外部サービスの契約。Bex の iroh/ネイティブ境界に該当。'
        if re.search(r'^(git|pullRequest|projectClone|scheduledTask|settings|sourceControl|threadPullRequest|usage|worktreeMcp|worktreeSetup|vcs)', path.name):
            return 'M3 の独立機能・契約。今回保留（会話から使う関連型は該当ファイルで個別に記録）。'
    if '/project/' in s and not path.name.startswith(('AgentSession',)):
        return '履歴取り込み以外の project 機能は今回の翻訳範囲外。残す Host のプロジェクト機能との接続点は importer に記録。'
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
                                    'rust': None, 'status': '未照合'})
            cases = []
            if kind == 'test':
                for match in re.finditer(r'\b(?:it|test)(?:\.(?:effect|scoped|live|skip|todo))?\s*\(\s*(["\x27`])([^\n]+?)\1', text):
                    cases.append({'name': match[2], 'line': text.count('\n', 0, match.start()) + 1, 'rust': None, 'status': '未移植'})
            records.append({'source': str(relative), 'sha256': hashlib.sha256(data).hexdigest(),
                            'lines': len(text.splitlines()), 'kind': kind,
                            'rust': None if why else destination(relative),
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
            inventory[index] = old[row['source']]
    if args.check:
        assert set(old) == {row['source'] for row in inventory}, 'Inventory contains missing or extra source files'
        assert previous['commit'] == COMMIT
        print(f'Fixed source inventory: {len(inventory)} files, hashes match.')
        return
    path.write_text(json.dumps({'commit': COMMIT, 'files': inventory}, ensure_ascii=False, indent=2) + '\n')
    intro = (DOCS / 'PORT_MAP.md').read_text().split('<!-- generated inventory -->')[0]
    out = [intro, '<!-- generated inventory -->\n', '\n## ファイル対応表\n\n予定の Rust パスは未翻訳行にも明記する。翻訳済みは production の呼出し経路・関数・移植テストを照合してから付ける。\n']
    for root in ROOTS:
        rows = [r for r in inventory if r['source'].startswith(root + '/')]
        production = [r for r in rows if r['kind'] == 'source']
        tests = [r for r in rows if r['kind'] == 'test']
        out += [f'\n### `{root}`\n\n本体 {len(production)} ファイル / {sum(r["lines"] for r in production):,} 行。テスト {len(tests)} ファイル / {sum(r["lines"] for r in tests):,} 行。testkit/fixture は別行で全件追跡する。\n',
                '\n| T3 のファイル（行数） | Rust のモジュール／テスト | 移植した T3 テスト | 状態・理由 |\n|---|---|---|---|\n']
        for r in rows:
            tests = '<br>'.join(r['tests']) or '—'
            rust = f'`{r["rust"]}`' if r['rust'] else '—'
            out.append(f'| `{r["source"]}` ({r["lines"]}) | {rust} | {tests} | {r["status"]}' + (f'：{r["reason"]}' if r['reason'] else '') + ' |\n')
    (DOCS / 'PORT_MAP.md').write_text(''.join(out))


if __name__ == '__main__':
    main()
