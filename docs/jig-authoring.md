# 治具の書き方 (AI 向け)

hookwatch の治具は、Human の摩擦から AI が書き、replay を使用して、Shadow で確定する

## 手順

1. 材料を読む: `hookwatch episodes --json --last 20`
   - cut_in: agent が動いている間に人が言ったこと。訂正そのもの
   - rework: 人がターンの間に直した file の差分
   - denied / tool_failure: 止められた、または失敗した tool の呼び出し
   - label: 治具の判定に人が付けた wrong / right
2. 同じ種類の摩擦をまとめ、1 つの治具で防げるかを決める
   - 決定的に書ける (正規表現、path、数) なら L1 の治具にする
   - 文脈が要るなら、今は治具にせず候補として残す (L2 は未実装)
3. 候補を jigs.json の形で書く。lifecycle は shadow にする
4. 確かめる: `hookwatch replay --file <候補.json>`
   - ran が多い: 普通の作業を止めてしまう。規則を狭める
   - 0 件: 過去に当たる例が無い。材料の episode に当たるかを確かめる
5. 人が読んで良ければ `$HOME/.hookwatch/jigs.json` に足す (足すのは人。tamper-guard が agent の書き込みを止める)

## 候補の形

```json
{
  "jigs": [
    {
      "id": "no-rm-rf-root",
      "description": "rm -rf で repo の外を消す",
      "rule": { "kind": "bash_regex", "pattern": "rm\\s+-rf\\s+(/|~)" },
      "action": "block",
      "lifecycle": "shadow",
      "message": "repo の外を消す操作は止める"
    }
  ]
}
```

rule の kind は bash_regex / content_regex / path_regex / test_weakening。
