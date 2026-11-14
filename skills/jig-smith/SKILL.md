---
name: jig-smith
description: hookwatch が記録した人の摩擦 (割り込み、手直し、拒否、失敗、ラベル) を読み、治具か規則の提案を 1 つ書いて hookwatch propose にかける。Use when: /jig-smith、「摩擦から治具を作って」「手直しから規則を起こして」。
---

# jig-smith

契約は repo の `docs/jig-smith.md` (入力・判断・出力・gate)。それを読んでから始める。

1. `hookwatch smith-input --repo <今の repo>` を読む
2. 同じ意図の episode をまとめ、意図・範囲・出口を決める。既に効いている治具や、却下された提案と重なれば `nothing` か `changed_since`
3. proposal の JSON を scratchpad に書き、`hookwatch propose <file> --dry-run` で gate を見る。落ちたら規則か cases を直す。直せない時は、落ちたまま `propose` して理由を残す
4. `hookwatch propose <file>` で置く
5. 報告: 提案の id、意図、出口と理由、gate の結果。accept / reject は人が端末で行う (agent からは通らない)

提案は 1 回に 1 つ。人の言葉と差分に無い意図を足さない。
