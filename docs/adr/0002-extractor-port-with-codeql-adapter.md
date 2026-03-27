# ADR-0002: `ExtractorPort` の背後に初期実装として CodeQL Adapter を置く

## メタ情報

| 項目 | 内容 |
|---|---|
| 承認日 | 2026-03-18 |
| 最終更新日 | 2026-03-27 |
| 改訂 | v0.4.7 |

> **注記**: メタ情報の `改訂` は本 ADR 自体の版番号であり、改訂履歴の `関連文書版` 列に記載される architecture.md / requirements.md / domain_model.md の版番号とは独立したバージョニング体系である。

## ステータス

承認済み

## コンテキスト

要件では CPG 抽出エンジンとして CodeQL を前提としつつ、代替エンジンの比較検討余地も残している。さらに、新言語追加時は CPG 抽出境界内の parser / normalizer / language profile 追加で対応できること、およびクリーン環境でも手動インストールなしで初回実行できることが要求される。

- `REQ-FUNC-001`〜`REQ-FUNC-007`
- `REQ-FUNC-031`, `REQ-FUNC-032`
- `REQ-NF-005`
- `REQ-NF-007`, `REQ-NF-009`
- 要件書 5章「PoC / 将来拡張で検証する項目」

ドメインモデル上も、`ExtractorPort` の外部公開契約は `SourceAnalysis` として定義され、`UnifiedCpg` はその内部公開言語として位置付けられている。これらの公開契約型は ADR-0001 の依存方向ルールに従い `ports` モジュールに配置する。

**本 ADR のスコープ**: 中核の判断は「`ExtractorPort` を定義し、初期アダプタとして CodeQL を採用する」ことである。以下の付随事項は `ExtractorPort` 採用に不可分なため、本 ADR で併せて決定する:

- **Tool Cache Port と Managed Tool Cache Adapter**: CodeQL bundle の bootstrap/検証/キャッシュは CPG Extraction コンテキスト内の独立したポート（`Tool Cache Port`）として定義し、`Managed Tool Cache Adapter` がこれを実装する。`ExtractorPort`（CPG 抽出ロジック）と `Tool Cache Port`（ツール取得・検証）は同一コンテキスト内で協調するが、責務は分離される（architecture.md §4.1, §4.2 参照）
- **CLI 主導 bootstrap**: `REQ-FUNC-031` の単一バイナリ要件により bootstrap の正本は CLI 側に置く必要がある。これはポート設計から自然に導かれる
- **GitHub Action の wrapper 限定**: `REQ-FUNC-032` で Action は prewarm/cache wrapper に留めると明記されており、bootstrap 経路の一貫性として本 ADR で拘束する

## 検討した選択肢

### 選択肢 A: CodeQL に直接依存する

アプリケーション層から CodeQL CLI / DB へ直接アクセスする。

- 利点:
  - 初期実装が最も簡単
  - 要件の「CodeQL 前提」に素直
- 欠点:
  - 性能問題や代替検討時の変更範囲が広い
  - `REQ-NF-005` の拡張性を損ないやすい

### 選択肢 B: `ExtractorPort` を定義し、初期アダプタとして CodeQL を採用する

抽出エンジンはポート背後へ隠蔽し、`ExtractorPort` の出力契約を `SourceAnalysis`（`UnifiedCpg` + `source_files` + 抑制コメント + 解析警告を束ねる集約ルート）に固定する。`source_files` は workspace-relative path をキーとする決定論的なソースファイルメタデータ対応表であり、下流コンテキストおよび LLM sidecar の `language` 解決の source of truth となる。

- 利点:
  - CodeQL 前提を守りながら将来差し替え可能
  - 言語ごとの差異をアダプタ層へ閉じ込められる
  - 下流コンテキストは `SourceAnalysis` の公開契約のみに依存し、抽出エンジンの詳細を知らない
- 欠点:
  - 抽象化の実装コストが増える
  - 初期 PoC の対象が増える

### 選択肢 C: 言語ごとに別エンジンを最初から使い分ける

Python/TS/Rust/Go で最適エンジンを変える。

- 利点:
  - 言語ごとの最適化余地がある
- 欠点:
  - 初期リリースで複雑すぎる
  - 決定論性と比較可能性を崩しやすい

## 判断

選択肢 B を採用する。本 ADR で決定する範囲は以下の通り:

1. **`ExtractorPort` の定義と CodeQL 初期アダプタの採用**: 抽出エンジンを `ExtractorPort` の背後に隠蔽し、出力契約を `SourceAnalysis`（`UnifiedCpg` + `source_files` + 抑制コメント + 解析警告を束ねる集約ルート）に固定する。初期アダプタとして CodeQL を採用する
2. **Tool Cache Port と Managed Tool Cache Adapter**: CodeQL bundle の bootstrap / 検証 / キャッシュは CPG Extraction コンテキスト内の独立したポート（`Tool Cache Port`）として定義し、`Managed Tool Cache Adapter` が実装する。`ExtractorPort` と `Tool Cache Port` は同一コンテキスト内で協調するが責務は分離される（architecture.md §4.1, §4.2 参照）
3. **CLI 主導 bootstrap**: `REQ-FUNC-031` の単一バイナリ要件により、bootstrap の正本は kalos CLI 側に置く
4. **GitHub Action の wrapper 限定**: `REQ-FUNC-032` に基づき、GitHub Action は managed tool cache の prewarm / cache wrapper に留め、bootstrap ロジックを Action 側に持たない

## 根拠

- CodeQL は初期実装として使うが、`ExtractorPort` の出力契約は `SourceAnalysis` に固定する。`SourceAnalysis` は `UnifiedCpg` に加え `source_files`（workspace-relative path をキーとし path 昇順で列挙される決定論的対応表）を含む。下流コンテキストは `SourceAnalysis` 内の `UnifiedCpg` を公開言語として参照し、LLM sidecar の `language` 解決は `source_files` を source of truth とする
- 外部依存の型情報・シグネチャ解決も extractor 境界内の language-specific resolver adapters へ閉じ込め、依存定義・lockfile・ローカル stub / metadata だけで解決する。解決失敗は `SourceAnalysis.warnings` として下流へ渡す
- CodeQL bundle は Managed Tool Cache Adapter が固定バージョン + SHA-256 検証付きで bootstrap / verify / cache し、CLI 利用者へ手動セットアップを要求しない（`REQ-FUNC-031`, `REQ-NF-009`）
- GitHub Action は managed tool cache を prewarm / restore/save する wrapper に留め、bootstrap の正本は kalos CLI 側に置く
- これにより、将来の性能問題や言語追加時の変更を抽出境界内へ閉じ込められる
- ドメインモデルの `LanguageExtension` と整合し、言語固有概念の差異をコアへ漏らさずに済む

## 帰結

### ポジティブ

- 下流コンテキストは CodeQL 非依存で保てる
- ローカル実行と GitHub Action が同じ bootstrap 経路（managed tool cache）を共有できる
- 性能 PoC に失敗しても代替エンジンへ移行しやすい
- 新言語追加時の変更面を限定できる

### ネガティブ

- `ExtractorPort` とマッパーの保守が必要
- CodeQL の表現差異を吸収する正規化ロジックが増える

### 制約

- CodeQL bundle は固定バージョンを managed tool cache へ初回取得し、SHA-256 で検証する。バージョンと checksum の正本は kalos リリースに同梱される managed bundle manifest とする（`REQ-NF-009`）
- managed bundle がキャッシュ済み（warm）かつ `--llm` 未使用であれば、オフライン環境でも `kalos check` が動作する（`REQ-NF-010`）
- bundle 未取得かつオフラインの場合は、bootstrap が必要であることを示す明確なエラーメッセージを出力し exit code 2 で終了する（`REQ-NF-010`）
- 外部シンボル解決は解析時に追加ネットワーク通信を行わず、依存定義・lockfile・ローカル stub / metadata だけを参照する（`REQ-FUNC-007`, `REQ-NF-009`）

### リスク

- CodeQL の実行時間が `REQ-NF-001` を満たさない可能性があるため、PoC に失敗した場合の代替案比較を継続する
  - **PoC 失敗の判定基準**: `architecture.md` QA-02 で定義する `bench-linux-x64`（4 vCPU / 16GB / SSD、managed CodeQL bundle warm、baseline cache empty）環境において、1 万 LOC 規模プロジェクトの全解析が 60 秒以内に完了しないこと（`REQ-NF-001`）
  - **再判断のトリガー**: 上記ベンチマーク PoC が閾値未達の場合、本 ADR を `再検討中` に戻し、要件書 5 章 #1「CodeQL 代替アダプタ比較を継続するか」および #4「NF-001 の 60 秒目標と CodeQL 抽出時間の両立可能性」に基づいて代替エンジンの比較評価を再開する

### 新言語追加時のスコープに関する注記

本 ADR が保証する「新言語追加時の変更面の限定」は、CPG 抽出境界内の parser / normalizer / language profile 追加を指す（`REQ-NF-005`, `architecture.md` QA-04）。外部シンボル解決のための language-specific resolver adapter は `architecture.md` で `Dependency Symbol Resolver Port` として別ポート化されており、要件書 5 章 #3 で個別の PoC 項目として扱われる。resolver adapter の設計判断は `REQ-FUNC-007` のスコープであり、本 ADR の判断範囲外である。

## 改訂履歴

> **凡例**: `関連文書版` は、当該改訂の時点で整合性を確認した各文書の版を示す（変更が導入された版ではなく、確認の対象とした版）。単独版（例: `v0.4.9`）は当該版のみを確認したことを、範囲表記（例: `v0.2.0–v0.2.8`）は当該範囲の変更を取り込み最終版で整合性を確認したことを表す。`arch` は architecture.md、`req` は requirements.md、`dm` は domain_model.md を指す。requirements.md / domain_model.md の版は architecture.md メタ情報の `入力` フィールドから導出する。v0.4.0 以前の改訂では domain_model.md の版を追跡していなかったため、`dm` はそれ以降のエントリに記載する。ADR 改訂日と各文書版の作成日は一致しない場合がある。同一日付の複数エントリは上から時系列順に記載する。

| 日付 | 変更概要 | 関連文書版 |
|---|---|---|
| 2026-03-18 | 初版承認 | arch v0.1.0 / req v0.1.0 |
| 2026-03-19 | `SourceAnalysis` 出力契約の明文化（`source_files` 含む）、Tool Cache Port の独立ポート化、PoC 失敗判定基準・再判断トリガー追記、新言語追加スコープの注記追加 | arch v0.2.5 / req v0.2.5 |
| 2026-03-22 | レビュー指摘解決: ADR 間参照整合、スコープ注記の明確化 | arch v0.4.0 / req v0.4.0 / dm v0.4.0 |
| 2026-03-27 | レビュー指摘解決: 公開契約型の `ports` 配置を ADR-0001 参照として追記し、`関連文書版` の凡例を改訂（requirements.md 追跡の追加・意味論の明確化） | arch v0.4.9 / req v0.4.7 / dm v0.4.6 |
| 2026-03-27 | レビュー指摘解決: `判断` セクションに付随判断（Tool Cache Port、CLI 主導 bootstrap、GitHub Action wrapper 限定）を明記し、判断境界を自己完結的に記述 | arch v0.4.9 / req v0.4.7 / dm v0.4.6 |
| 2026-03-27 | `関連文書版` に domain_model.md（`dm`）追跡を追加（本 ADR が依拠する SourceAnalysis・UnifiedCpg・source_files 契約の出所を明示） | arch v0.4.16 / req v0.4.10 / dm v0.4.11 |
| 2026-03-27 | provenance 整備: 凡例を ADR-0003..0005 と整合（単独版/範囲表記の意味論・同日エントリ順序の注記を追加） | arch v0.4.18 / req v0.4.11 / dm v0.4.11 |
| 2026-03-27 | `関連文書版` を requirements.md v0.4.13 に同期（ADR 本文の変更なし） | arch v0.4.20 / req v0.4.13 / dm v0.4.12 |
| 2026-03-27 | `関連文書版` を architecture.md v0.4.22 / requirements.md v0.4.14 に同期（ADR 本文の変更なし） | arch v0.4.22 / req v0.4.14 / dm v0.4.12 |
| 2026-03-27 | `関連文書版` を architecture.md v0.4.23 / domain_model.md v0.4.13 に同期（ADR 本文の変更なし） | arch v0.4.23 / req v0.4.14 / dm v0.4.13 |
