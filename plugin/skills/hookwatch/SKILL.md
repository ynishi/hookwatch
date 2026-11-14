---
name: hookwatch
description: hookwatch (この repo で有効な plugin) の記録を読んで、作業の摩擦 (割り込み、手直し、permission 待ち、拒否、失敗) と治具の判定を報告する。Use when: 「hookwatch」「摩擦を見て」「どこで止まった」「治具の判定」「stats」、session の開始時に hookwatch の知らせが出た時。
---

# hookwatch

この repo では hookwatch の plugin が有効で、全ての hook の event が人の手元の ledger に記録され、治具 (jig) が tool の呼び出しを判定している。

## 読む (agent が回してよい)

- `hookwatch status`: ledger の大きさ、最後の prune、open の提案、ラベルの無い判定
- `hookwatch stats --repo <この repo>`: 割り込み、待ち時間、拒否、失敗、人の手直し (rework)、治具ごとの精度
- `hookwatch why [id]`: 判定の一覧と、1 件の根拠と tool の入力
- `hookwatch episodes --repo <この repo> [--json]`: 摩擦 1 件ごとのゴールと直前の呼び出し
- `hookwatch replay [--file <候補>]`: 治具を過去の呼び出しに流して当たりを数える
- 治具を作る: skill `jig-smith`

ledger の file (`$HOME/.hookwatch/`) を直接開かない。読む口は上のコマンド。

人が見る管理画面は、チャットの横の pane (`/hookwatch-pane` で開く)。判定と提案の行のボタンは、人が端末で打つコマンドをクリップボードに写すだけ。

## 人だけが行う (agent からは端末の検査で止まる)

`accept` / `reject` (提案の判断)、`put-jigs` (治具の file を効かせる)、`prune` (ledger を縮める)、`override <id> --right|--wrong` (判定へのラベル)。agent はコマンドの 1 行を人に示し、自分では打たない。治具の判定に止められた時は、形を変えて試し直さず、止められたことを報告する。

## 知らせの読み方

session の開始時に `hookwatch: ...` の行が出たら、その内容 (prune が要る、open の提案、ラベルの無い判定、コマンドや gitleaks が無い) を User に伝え、上の「人だけが行う」のどれに当たるかを示す。
