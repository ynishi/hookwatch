# 治具の lifecycle

治具は Shadow から始め、人のラベルで精度を確かめてから enforce に上げる。

| 値 | 意味 |
|---|---|
| `shadow` | 判定を記録するだけで、agent には何もしない |
| `enforce` | block なら止め、nudge なら注意を添える |
| `retired` | 判定しない |
