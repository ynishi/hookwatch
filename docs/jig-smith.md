# Jig Smith の契約

Jig Smith は、人の摩擦から治具か規則を提案する AI の役。契約 (入力・判断・出力・gate) の正本は plugin の skill: [`plugin/skills/jig-smith/SKILL.md`](../plugin/skills/jig-smith/SKILL.md)。他の repo で plugin を使う時も、AI はそれを読む。

人の判断: `hookwatch proposals [id]` で見て、`hookwatch accept <id>` か `hookwatch reject <id> --why ...`。どちらも端末が要るので、agent からは通らない。accept した jig は Shadow で入る。enforce に上げるのは、ラベルの精度を見てから人が行う。
