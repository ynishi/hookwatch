# Jig Smith の契約

Jig Smith は、人の摩擦から治具か規則を提案する AI の役。hookwatch は材料を渡し、提案を検査し、人の判断を記録する。提案は、人が accept するまで何も変えない。

## 入力

`hookwatch smith-input --repo <repo>` の JSON 1 つ。

- `episodes`: 摩擦 1 件ごとに、種類 (cut_in / rework / denied / tool_failure / label)、ゴール (直前の人のプロンプト)、直前の tool の呼び出し、中身 (人の言葉、差分、エラー、ラベル)、`conversation` (前後の人と agent の発言)
- `jigs`: いま効いている治具
- `proposals`: これまでの提案 (却下とその理由を含む)
- `rule_places`: 文書の規則を置ける file

## 判断

1. 摩擦の意図を書く。人が何を嫌ったか、何を望んだかを、人の言葉と差分から読む
2. 範囲を決める
   - `lasting`: 「これからは」の意図
   - `this_task`: 今の作業だけ
   - `ambiguous`: 意味か範囲が曖昧
   - lasting 以外は `nothing` しか出せない
3. 出口を決める

| 意図 | candidate |
|---|---|
| 単純・全体に効く・例外なし・tool の呼び出しだけで判定できる | `jig` (regex か command。Shadow で入る) |
| 意味の判断が要る | `hook_l2_pending` (判定の質問だけを書く) |
| 守ってほしいが呼び出しでは判定できない | `rule` (`rule_places` の 1 か所への追記) |
| 範囲が曖昧、今回だけ、既に効いている治具で足りる | `nothing` (理由を書く) |

4. 前に却下された案と同じ意図なら、`changed_since` に何が変わったかを書く

## 出力

proposal の JSON 1 つを file に書き、`hookwatch propose <file>` を回す。まず `--dry-run` で gate を確かめてよい。

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

## gate (jig だけ)

- cases は 3〜9 件で、hit と pass の両方を含み、hit のうち 1 件以上は episode から取る
- cases がすべて期待どおり
- replay: 普通に実行された過去の呼び出しのうち、hit の case で確かめていない当たりは、block なら 0 件、nudge なら当たり全体の 10% 未満
- 落ちた提案は理由つきで rejected として残る

## 人の判断

`hookwatch proposals [id]` で見て、`hookwatch accept <id>` か `hookwatch reject <id> --why ...`。どちらも端末が要るので、agent からは通らない。accept した jig は Shadow で入る。enforce に上げるのは、ラベルの精度を見てから人が行う。
