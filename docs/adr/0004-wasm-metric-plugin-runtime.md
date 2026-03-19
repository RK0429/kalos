# ADR-0004: ユーザー定義メトリクス拡張に WASM ベースのプラグイン境界を採用する

## ステータス

承認済み

## コンテキスト

要件では、ユーザーが独自メトリクスを追加できる拡張機構が求められている。v1 ではユーザー入力面は `.kalos.toml` の `[[plugins]] { path, sha256 }` とし、`path` は解決済み `WorkspaceRoot` 基準で解釈する。そこから解決した内部表現 `plugin_manifest` を Plugin Host の正本とする。外部の配布パッケージ形式は将来拡張へ残す。

- `REQ-FUNC-012`
- `REQ-NF-006`
- `REQ-NF-003`

さらに、対応 OS が Linux / macOS / Windows にまたがるため、配布・互換性・安全性を同時に考える必要がある。

## 検討した選択肢

### 選択肢 A: Rust の組み込み trait 実装だけを許可する

- 利点:
  - 最も高速で型安全
  - 実装が単純
- 欠点:
  - ユーザーがバイナリ再ビルドなしに追加できない
  - `REQ-FUNC-012` の期待を満たしにくい

### 選択肢 B: 動的ライブラリプラグイン

- 利点:
  - 実行時ロードが可能
  - ネイティブ性能を得やすい
- 欠点:
  - ABI とクロスプラットフォーム配布が難しい
  - 安全なサンドボックス化が難しい

### 選択肢 C: WASM ランタイム上でメトリクス SPI を公開する

- 利点:
  - クロスプラットフォームに配布しやすい
  - ホスト機能を絞れば安全性を高めやすい
  - コアから拡張点を明確に分離できる
- 欠点:
  - ランタイムコストがある
  - SPI 設計が必要

## 判断

選択肢 C を採用する。

## 根拠

- kalos 本体は ADR-0001 に従い単一バイナリとして配布する。ユーザー定義メトリクスプラグインは **kalos バイナリとは別に配布される外部 WASM モジュール** であり、`.kalos.toml` の `[[plugins]] { path, sha256 }` へ登録することでバイナリ再ビルドなしに追加できる。ホストはこれを `WorkspaceRoot` 基準の決定論的な内部表現 `plugin_manifest` へ正規化して扱う。WASM はクロスプラットフォームなバイトコード形式のため、プラグイン作成者は OS/arch ごとのビルドを持つ必要がない
- `Configuration` は `[[plugins]]` の `path` を `WorkspaceRoot` 基準で canonicalize し、`WorkspaceRoot` 外参照または `sha256` 構文不正を設定エラー（exit code 2）として扱う。Plugin Host はこの検証を通過した `plugin_manifest` だけを入力に受け取り、実行時の失敗境界と設定不正の境界を分離する
- ホストが渡すのは additive-only な `CpgSubgraph` の read-only view と `MetricConfig` だけに絞り、ネットワークやファイル書込は許可しない。Plugin Host は各 plugin metric を `MetricDefinition.level` に一致する各 `ScopeId` ごとに 1 回ずつ評価し、入力には `UnifiedCpg.subgraph(scope_id)` を渡す。function/module metric は該当 scope ごとに 1 回ずつ、project metric は正規形 `ScopeId(level = Project, qualified_name = "<project>", file_path = ".")` に対して 1 回だけ評価する。プラグインはロード時に stable `metric_id`, `level`, `name`, `description` を持つ `MetricDefinition` を登録し、v1 では `participation = ReportOnly`、`rule_binding = None` とする
- Plugin Host は `plugin_manifest` を `workspace_relative_path` 昇順でロードし、`metric_id` のグローバル一意性を検証する。組み込みメトリクスまたは先行ロード済みプラグインと `metric_id` が衝突したモジュールは deterministic なロード失敗として扱い、warning を出してスキップする
- プラグインファイル読込失敗、checksum 不一致、SPI version 不一致、`metric_id` 衝突、タイムアウト、メモリ超過は warning + skip とし、当該プラグインのみを失敗させる。aggregate CPU time budget 超過時は残りプラグインを warning 付きでスキップする。いずれも `stderr` / 構造化ログへ運用警告を出し、v1 の診断・スコア・Exit code 契約は変更しない
- `REQ-NF-003` を守るため、評価 SPI は pure function (`CpgSubgraph + MetricConfig -> MetricValue`) とし、乱数・時刻・外部 I/O を禁止する
- 実行ごとに `cpu_time_budget = 50ms`、`linear_memory_limit = 64MiB`、実行全体では Metrics stage budget の内数として `aggregate_cpu_time_budget = 3s`（全解析）/ `0.5s`（diff mode）を既定上限として適用する
- per-invocation `cpu_time_budget` と `aggregate_cpu_time_budget` はいずれも WASM fuel metering で制御する。壁時間に依存しないことで、同一入力に対するプラグインの成否判定が `REQ-NF-003` の決定論性を維持する
- diff mode では、現在の実行で失敗またはスキップされたプラグインの baseline cache 済み `MetricValue` を最終出力から除外する。これにより stale な report-only plugin metric の部分的再利用を防ぐ
- **v1 SPI 互換性契約**: Plugin Host は SPI version `kalos-metric-spi-v1` を定義する。WASM モジュールは custom section `kalos_spi_version` に SPI version 文字列（例: `kalos-metric-spi-v1`）を宣言する。ホストはロード時にこの値を検証し、以下の規則で互換性を判定する
  - SPI version が完全一致する場合のみロードを許可する
  - custom section が存在しない、または値が不一致の場合はロード失敗として扱い、当該プラグインの評価をスキップする。運用警告を `stderr` / 構造化ログへ出力し、v1 の診断・スコア・Exit code 契約には影響させない
  - SPI version の更改（`kalos-metric-spi-v2` 等）はバイナリ互換を保証しない破壊的変更として扱い、新規 ADR で決定する
- 組み込みメトリクスは引き続きネイティブ実装とし、高頻度パスの性能を守る

## 帰結

### ポジティブ

- プラットフォーム差異の小さい拡張機構を持てる
- サンドボックス境界を設けやすい
- コアの変更面を抑えたままメトリクス追加を許可できる
- 決定論的な評価経路に外部プラグインを載せても、純粋関数契約で再現性を維持できる
- kalos 本体の単一バイナリ配布（ADR-0001）を崩さずにプラグイン拡張を提供できる
- 時間・メモリ上限を契約化することで `REQ-NF-001/002` との整合を説明しやすい
- v1 は report-only metric に限定することで、RuleId / score / exit code 契約を増やさずに拡張点を提供できる

### ネガティブ

- SPI version `kalos-metric-spi-v1` の保守が必要であり、破壊的変更時は新 SPI version と移行計画を ADR で決定する
- 実行性能の測定が不可欠
- プラグインは kalos バイナリとは別に配布・管理する必要がある（v1 では `.kalos.toml` の `[[plugins]]` に path と checksum を登録し、ホストが `WorkspaceRoot` 基準の `plugin_manifest` へ正規化する）
- タイムアウト、メモリ上限、aggregate CPU time budget 超過時のプラグインは `MetricValue` を返せず、ホストは運用警告を記録したうえで当該プラグイン評価または残り評価を打ち切る。v1 の診断・スコア・Exit code は既存契約のまま維持する。diff mode では baseline 断片に残る当該プラグインの `MetricValue` も最終出力から除外する

### リスク

- WASM オーバーヘッドが大きい場合、初期リリースでは組み込みメトリクス中心で運用し、外部プラグインを experimental 扱いにする可能性がある
- 既定上限（50ms / 64MiB / 3s / 0.5s）が厳しすぎる、または緩すぎる可能性があるため、PoC で計測し必要なら v1.1 で再調整する
