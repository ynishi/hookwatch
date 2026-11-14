# hookwatch

Friction-measured hook engine (jigs) for coding agents

A prototype: `[package.metadata.caps] stage = "prototype"` in Cargo.toml marks it, and it is not published (`publish = false`).

## What’s hookwatch?

hookwatch は Claude Code の hook として動き、agent の作業を 2 つの面から見る。

- 治具 (jig): tool の呼び出しを、実行の前に決定的な規則で判定する。止める (block)、注意を添える (nudge)、記録だけする (shadow) のどれかを選べる
- ストップウォッチ: 全ての hook event を Friction Ledger (SQLite) に記録し、人の手間を数える。割り込み (cut-in)、permission と質問の待ち時間、Deny、tool の失敗、人の手直し (Rework)

治具は人の判断で研ぐ。誤った判定には人がラベルを付け、その数が治具の精度になる。

## Install

hookwatch は Claude Code の plugin として、使う repo ごとに有効にする (user 全体には入れない)。

1. command を入れる: この repo で `just install` (`cargo install --locked --path .`)。gitleaks も要る (ledger に入る credential を伏せる)
2. marketplace を足す (1 回だけ): `claude plugin marketplace add ynishi/hookwatch` (手元の checkout から開発する時は `claude plugin marketplace add <この repo の path>`。repo の `plugin/` を直接読むので、直したら `/reload-plugins` で反映される)
3. 使う repo で有効にする: その repo で `claude plugin install hookwatch@hookwatch --scope local`

plugin の hooks は `plugin/scripts/hook.sh` を経て `hookwatch hook` を呼ぶ。command か gitleaks が無い時は記録せずに通し、session の開始時にそれを知らせる。plugin には skill `hookwatch` (記録の読み方) と `jig-smith` (摩擦から治具を提案する) が入る。管理画面はチャットの横の pane で、`/hookwatch-pane` で開く (幅が 144 列以上なら session の開始時に開く)。判定と提案の行のボタンは、人が端末で打つコマンドをクリップボードに写す (agent からは打てないコマンドなので、mod も実行しない)。

## How to use.

| command | 何をするか |
|---|---|
| `hookwatch hook` | hook として呼ばれる。event の JSON を stdin で受ける |
| `hookwatch stats` | ストップウォッチの数字と、治具ごとの発火数・精度 |
| `hookwatch why [id]` | 判定の一覧、または 1 件の根拠と tool の入力 |
| `hookwatch override <id> --wrong\|--right` | 判定に人のラベルを付ける |
| `hookwatch events` | 記録した event を古い順に出す |
| `hookwatch jigs` | 効いている治具の一覧 |
| `hookwatch put-jigs <file>` | 治具の file を効かせる (前の file は写しを残す)。端末が要る (人だけ)。`just jigs` は repo の `jigs/jigs.json` でこれを呼ぶ |
| `hookwatch status` | ledger の大きさと、人の判断を待つ物 |
| `hookwatch episodes` / `smith-input` / `propose` / `proposals` | 摩擦の材料と、治具の提案 (skill `jig-smith`) |
| `hookwatch accept` / `reject` / `prune` | 提案の判断、ledger を縮める。端末が要る (人だけ) |
| `hookwatch install` | plugin を使わずに settings に足す時の hooks の JSON を出す |

記録は `$HOME/.hookwatch/ledger.db`、治具の追加と lifecycle の上書きは `$HOME/.hookwatch/jigs.json` に置く (`HOOKWATCH_HOME` で場所を変えられる)。

## 組み込みの治具

| id | 既定 | 何を見るか |
|---|---|---|
| no-verify | enforce | git の hook を飛ばす `--no-verify` |
| secret-in-file | enforce | 秘密鍵や cloud の credential を file に書くこと |
| tamper-guard | enforce | hook の設定と hookwatch 自身の状態への書き込み (読むのは止めない) |

## Rust と script の分け方

- hookwatch (Rust) が持つのは仕組みだけ: 記録、手直しの計測、治具の起動と時間の計測、Shadow / enforce、replay、決める者の確認
- 何を当たりとするかは治具 (`jigs/` の Python script) に置く。例: `jigs/tests_weakened.py` (test の assert を減らす編集)、`jigs/hotspots.py` (何度も書いて回した script を、ターンの終わりに正式な道具の候補として知らせる)。書き方は [docs/jig-authoring.md](docs/jig-authoring.md)
- 効いている治具の script は `$HOME/.hookwatch/jigs/` の写しだけが動く (`just jigs` か `accept` が写す)。提案の accept / reject は、人か、人が `$HOME/.hookwatch/deciders.json` に書いた役の agent (`hookwatch:jig-judge`) が行う。label・put-jigs・prune は人だけ

## Rework の数え方

agent のターンが終わった時 (Stop) に、そのターンで agent が Edit / Write した file の中身を写しとして取る。次のプロンプトの時に今の中身と比べ、違えば人がターンの間に直したとして、変わった行数を記録する。

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
