---
name: jig-smith
description: hookwatch が記録した人の摩擦 (割り込み、手直し、拒否、失敗、ラベル) を読み、治具か規則の提案を 1 つ書いて hookwatch propose にかける。Use when: /jig-smith、「摩擦から治具を作って」「手直しから規則を起こして」。
---

# jig-smith

人の摩擦から、治具か規則の提案を 1 つ作る。hookwatch は材料を渡し、提案を検査し、人の判断を記録する。提案は、人が accept するまで何も変えない。

## 手順

1. `hookwatch smith-input --repo <今の repo>` を読む
2. 同じ意図の episode をまとめ、意図・範囲・出口を決める (下の「判断」)
3. proposal の JSON を session の scratchpad に書き、`hookwatch propose <file> --dry-run` で gate を見る。落ちたら規則か cases を直す。直せない時は、落ちたまま `propose` して理由を残す
4. `hookwatch propose <file>` で置く
5. 報告: 提案の id、意図、出口と理由、gate の結果。accept / reject は人が Claude Code の外の端末で行う (agent からは通らない)

提案は 1 回に 1 つ。人の言葉と差分に無い意図を足さない。

## 入力 (smith-input の JSON)

- `episodes`: 摩擦 1 件ごとに、種類 (cut_in / rework / denied / tool_failure / label)、ゴール (直前の人のプロンプト)、直前の tool の呼び出し、中身 (人の言葉、差分、エラー、ラベル)、`conversation` (前後の人と agent の発言)。rework には `source_call` (人が直す前に agent が書いた呼び出しそのもの)
- `jigs`: いま効いている治具
- `proposals`: これまでの提案 (却下とその理由を含む)
- `rule_places`: 文書の規則を置ける file

## 判断

1. 摩擦の意図を書く。人が何を嫌ったか、何を望んだかを、人の言葉と差分から読む。人が直さずに残した所も証拠にする
2. 範囲を決める: `lasting` (「これからは」)、`this_task` (今の作業だけ)、`ambiguous` (意味か範囲が曖昧)。lasting 以外は `nothing` しか出せない
3. 出口を決める

| 意図 | candidate |
|---|---|
| 単純・全体に効く・例外なし・tool の呼び出しだけで判定できる | `jig` (regex か command。Shadow で入る) |
| 意味の判断が要る | `hook_l2_pending` (判定の質問だけを書く) |
| 守ってほしいが呼び出しでは判定できない | `rule` (`rule_places` の 1 か所への追記) |
| 範囲が曖昧、今回だけ、既に効いている治具で足りる | `nothing` (理由を書く) |

4. 前に却下された案と同じ意図なら `changed_since` に何が変わったかを書く。効いている治具を置き換える時は `replaces` にその id を書く

## 出力 (proposal の JSON)

```json
{
  "episodes": [{"kind": "rework", "ts_ms": 0}],
  "intent": "人が望んだこと",
  "scope": "lasting",
  "reason": "この出口にした理由と、他の出口にしなかった理由",
  "candidate": {"kind": "jig", "jig": {"id": "...", "description": "...", "rule": {"kind": "bash_regex", "pattern": "..."}, "action": "nudge", "lifecycle": "shadow", "message": "...", "when": {"tools": ["Bash"]}}},
  "cases": [
    {"tool_name": "Bash", "tool_input": {"command": "..."}, "expect": "hit", "from_episode": true, "note": "元の摩擦"},
    {"tool_name": "Bash", "tool_input": {"command": "..."}, "expect": "pass", "note": "当たってはいけない普通の作業"}
  ]
}
```

rule の kind: `bash_regex` / `content_regex` / `path_regex` / `test_weakening` / `command` (`argv` を hookwatch が起動し、payload を stdin に渡す。exit 2 が当たりで stderr が根拠。argv の先頭の `$HOME/` は展開される)。`when` (`tools`、`path` の正規表現) で起動する呼び出しを絞る。episode から取る case は `source_call` をそのまま写す (replay の当たりと完全一致で照らすため)。

## gate (jig だけ。propose が検査する)

- cases は 3〜9 件で、hit と pass の両方を含み、hit のうち 1 件以上は episode から取る
- cases がすべて期待どおり
- replay: 普通に実行された過去の呼び出しのうち、hit の case か人の right のラベルで確かめていない当たりは、block なら 0 件、nudge なら当たり全体の 10% 未満
- 落ちた提案は理由つきで rejected として残る
