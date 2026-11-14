# 治具の書き方 (AI 向け)

hookwatch の治具は、Human の摩擦から AI が書き、replay を使用して、Shadow で確定する。

役割の分け方: hookwatch (Rust) は、治具を動かし、時間を測り、Shadow / enforce を切り替え、決める者を確かめる仕組みだけを持つ。何を当たりとするかの判断は治具に置く。判断を直すのに Rust の build は要らない。

## 手順

1. 材料を読む: `hookwatch episodes --json --last 20`
   - cut_in: agent が動いている間に人が言ったこと。訂正そのもの
   - rework: 人がターンの間に直した file の差分
   - denied / tool_failure: 止められた、または失敗した tool の呼び出し
   - label: 治具の判定に人が付けた wrong / right
2. 同じ種類の摩擦をまとめ、1 つの治具で防げるかを決める
   - 正規表現 1 つで書ける: `bash_regex` / `content_regex` / `path_regex` (起動が要らず速い)
   - それ以外: `command` (Python などの script)。文脈が要る判断、Ledger を読む判断、ターンの終わりの判断はこちら
3. 候補を jigs.json の形で書く。lifecycle は shadow にする。script は repo の `jigs/` に置き、`jigs/test_<名前>.py` に unittest を付ける
4. 確かめる: `hookwatch replay --file <候補.json>` と `just check`
   - ran が多い: 普通の作業を止めてしまう。規則を狭める
   - 0 件: 過去に当たる例が無い。材料の episode に当たるかを確かめる
5. `hookwatch propose` で置く。accept するのは人か、人が `deciders.json` に書いた役の agent (`hookwatch:jig-judge`)

## command の治具の約束

- 起動: argv のとおり。hook の入力 (Claude Code の event の JSON) が stdin に来る
- 返し方: exit 0 = 当たりなし、exit 2 = 当たり (stderr の 1 行が証拠)、それ以外と時間切れ = 治具の失敗
- 環境変数: `HOOKWATCH_BIN` (hookwatch 自身の path)、`HOOKWATCH_EVENT_TS_MS` (判定している event の時刻。replay では記録の時刻)
- Ledger を読む: `$HOOKWATCH_BIN events --json --repo <repo> --until-ms $HOOKWATCH_EVENT_TS_MS`。1 行 1 JSON (`ts_ms`、`session`、`kind`、`tool`、`payload`)、古い順。絞り込みは `--kinds`、`--tool`、`--session`、`--last`
- 置き場: 効いている治具の script は `$HOME/.hookwatch/jigs/<治具の id>/` の写しだけが動く。`put-jigs` と `accept` がそこに写す (repo の file を後から変えても、効いている判定は変わらない)。repo の path のままの治具は、効いている間は「失敗」になる。replay `--file` と propose の gate だけは repo の path で動く

## 形

```json
{
  "jigs": [
    {
      "id": "hotspots",
      "description": "scripts written and run again and again",
      "rule": { "kind": "command", "argv": ["python3", "-I", "$HOME/projects/hookwatch/jigs/hotspots.py"], "timeout_ms": 5000 },
      "action": "nudge",
      "lifecycle": "shadow",
      "message": "make these canonical tools",
      "when": { "events": ["Stop"] },
      "repeat": "once"
    }
  ]
}
```

- `when.events`: 見る hook の event。省略は `["PreToolUse"]`。PreToolUse 以外を見られるのは command の治具だけで、action は nudge だけ (止められるのは呼び出しの前だけ)
- `when.tools` / `when.path`: tool 名と、対象の file の path の正規表現。合わない呼び出しでは script を起動しない
- 当たりの伝え方: PreToolUse の block は exit 2 で止める。nudge は PreToolUse・PostToolUse・UserPromptSubmit では agent に、Stop と SessionStart では人に (systemMessage) 伝える
- `repeat: once`: 同じ治具が同じ証拠をもう出していれば、2 回目からは伝えない (続いている状態を毎ターン言わないため)。証拠は毎回同じ文字列にする

rule の kind は bash_regex / content_regex / path_regex / command。`test_weakening` は無くなった (`jigs/tests_weakened.py` の command の治具になった)。
