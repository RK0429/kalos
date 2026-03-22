# ADR-0004: ユーザー定義メトリクス拡張に WASM ベースのプラグイン境界を採用する

## メタ情報

| 項目 | 内容 |
|---|---|
| 承認日 | 2026-03-18 |
| 最終更新日 | 2026-03-22 |
| 改訂 | v0.4.5 |

## ステータス

承認済み

## コンテキスト

要件では、ユーザーが独自メトリクスを追加できる拡張機構が求められている。v1 ではユーザー入力面は `.kalos.toml` の `[[plugins]] { path, sha256 }` とし、`path` は解決済み `WorkspaceRoot` 基準で解釈する。そこから解決した内部表現 `plugin_manifest` を Plugin Host の正本とする。外部の配布パッケージ形式は将来拡張へ残す。

- `REQ-FUNC-012` — メトリクス定義のプラグイン拡張
- `REQ-NF-001` — 中規模プロジェクトの全階層解析時間（60s 以内）
- `REQ-NF-002` — 差分解析の実行時間（10s 以内）
- `REQ-NF-003` — 決定論的評価
- `REQ-NF-006` — メトリクス定義の追加

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
- **v1 モジュール ABI / ホスト契約**: v1 で受け入れるプラグインモジュールは以下の条件を満たすこと
  - **target**: `wasm32-unknown-unknown`（WASI 不使用）。ファイルシステム・ネットワーク・クロック等の WASI API は一切インポートを許可しない。これにより `REQ-NF-003` の決定論性を保証し、サンドボックス面を最小化する
  - **ptr/len エンコーディング契約**: ホスト⇔プラグイン間で `_ptr`/`_len` サフィックスを持つ引数ペアは、プラグインの線形メモリ上のバイトオフセット（`_ptr`: `u32`）とバイト長（`_len`: `u32`）を表す。ホストは `_len` バイトだけを読み書きし、範囲外アクセスはトラップとする。引数が指すデータの形式は以下の 2 種に分かれる:
    - **文字列パラメータ**（`id`, `name`, `desc`, `key`, `metric_id`）: UTF-8 エンコードされた文字列。NUL ターミネータは含めない
    - **ScopeId パラメータ**（`scope`）: 後述の「ScopeId 直列化契約」に従うバイナリレイアウト（UTF-8 文字列ではない）
  - **ScopeId 直列化契約**: `scope_ptr`/`scope_len` が指す領域は、`ScopeId`（domain_model.md §3.2）の以下の Little-Endian バイナリレイアウトである。`scope_len` はレイアウト全体のバイト長（`= 4 + 4 + qn_len + 4 + fp_len`）を表す:
    - レイアウト: `[level: u32, qn_len: u32, qn_bytes: [u8; qn_len], fp_len: u32, fp_bytes: [u8; fp_len]]`
    - `level`: `AnalysisLevel` 列挙値（`0` = Function, `1` = Module, `2` = Project）。`metric_register` の `level` 引数と同じ値域
    - `qn_bytes`: `ScopeId.qualified_name` の UTF-8 バイト列（長さは `qn_len`）
    - `fp_bytes`: `ScopeId.file_path` の UTF-8 バイト列（`WorkspaceRoot` 相対、長さは `fp_len`）
    - ホストは `kalos_plugin_evaluate` 呼び出し前に `kalos_plugin_alloc` でプラグインの線形メモリ上に `metric_id`（UTF-8 文字列）と `scope`（本バイナリレイアウト）を書き込み、それぞれのポインタと長さを引数として渡す。プラグインは受け取った `scope_ptr`/`scope_len` をホスト関数（`cpg_node_count` 等）にそのまま引き渡す。ホストは各ホスト関数呼び出し時に本レイアウトから `ScopeId` を復元する
  - **read ヘルパー戻り値契約**: `cpg_read_node`、`cpg_read_edge`、`config_read` の `i32` 戻り値は以下の共通セマンティクスに従う
    - `>= 0`: 成功。戻り値は `buf_ptr` から書き込まれた実バイト数を表す
    - `-1`: バッファ不足。`buf_len` が必要なデータサイズより小さい場合に返す。`buf_ptr` の内容は未定義とする。プラグインはより大きなバッファを `kalos_plugin_alloc` で確保して再呼び出しできる
    - `-2`: 範囲外インデックスまたは存在しないキー。`cpg_read_node`/`cpg_read_edge` で `index >= count` の場合、または `config_read` で未定義のキーの場合に返す。`buf_ptr` の内容は未定義とする
  - **host exports（ホスト→プラグイン）**: ホストは以下の関数をプラグインにエクスポートする
    - `cpg_node_count(scope_ptr, scope_len) -> u32` — 対象スコープのノード数を返す
    - `cpg_edge_count(scope_ptr, scope_len) -> u32` — 対象スコープのエッジ数を返す
    - `cpg_read_node(scope_ptr, scope_len, index: u32, buf_ptr, buf_len) -> i32` — ノードデータを線形メモリへ書き込む。戻り値は read ヘルパー戻り値契約に従う
    - `cpg_read_edge(scope_ptr, scope_len, index: u32, buf_ptr, buf_len) -> i32` — エッジデータを線形メモリへ書き込む。戻り値は read ヘルパー戻り値契約に従う
    - `config_read(key_ptr, key_len, buf_ptr, buf_len) -> i32` — `MetricConfig` のキーに対応する値を線形メモリへ書き込む。戻り値は read ヘルパー戻り値契約に従う
    - `metric_register(id_ptr, id_len, level: u32, name_ptr, name_len, desc_ptr, desc_len) -> i32` — `MetricDefinition` を登録する。`id` は `MetricDefinition.id`（グローバル一意）、`level` は `AnalysisLevel`（0=Function, 1=Module, 2=Project）、`name`/`desc` は人間可読な名前と説明。v1 では `participation = ReportOnly`, `rule_binding = None` が暗黙に設定される（domain_model.md §3.2 参照）。成功時 0、重複 ID 時 -1 を返す
  - **plugin exports（プラグイン→ホスト）**: プラグインは以下の関数をエクスポートする
    - `kalos_plugin_init() -> i32` — 初期化。`metric_register` ホスト関数を呼び出して `MetricDefinition` を 1 つ以上登録する。1 モジュールが複数の `MetricDefinition` を登録できる（各登録は独立した `metric_id` を持つ）。成功時 0、失敗時非 0 を返す
    - `kalos_plugin_evaluate(metric_id_ptr, metric_id_len, scope_ptr, scope_len) -> i64` — 指定メトリクスを指定スコープに対して評価し `MetricValue` を返す。`metric_id` は `kalos_plugin_init` 内の `metric_register` で登録済みの `MetricDefinition.id` でなければならない。ホストは登録された各 `metric_id` と `MetricDefinition.level` に一致する各 `ScopeId` の組み合わせについて本関数を 1 回ずつ呼び出す。ホストは登録済みの `metric_id` のみを渡すことを保証するため、未登録の `metric_id` が渡されることは正常動作では発生しない。万一ホスト実装の不具合により未登録の `metric_id` が渡された場合、ホストは当該呼び出しをプラグイン評価失敗として扱い、`MetricValue` を生成せず、`stderr` / 構造化ログへ warning を出力する。プラグイン側の戻り値は無視する。**スカラー戻り値エンコーディング**（WASM 値スタック上の `i64` 値であり、線形メモリレイアウトではない）: 下位 32 ビット（ビット 0–31）が `raw_value` の IEEE 754 binary32 ビットパターン（`i32.reinterpret_f32` 相当）、上位 32 ビット（ビット 32–63）が `normalized_risk` の IEEE 754 binary32 ビットパターン。ホストは受け取った binary32 値を `f64` へ拡張したのち、以下の **invalid-value contract** を適用する:
      - `normalized_risk` が `NaN` または `±Inf` の場合: 当該呼び出しをプラグイン評価失敗として扱い、`MetricValue` を生成しない。`stderr` / 構造化ログへ warning を出力する
      - `normalized_risk` が有限だが `[0.0, 1.0]` の範囲外の場合: `clamp(normalized_risk, 0.0, 1.0)` で範囲内に収め、`stderr` / 構造化ログへ warning を出力したうえで処理を続行する
      - 上記の検証を通過した値に対し、domain_model.md の丸め契約（小数第 6 位 round-half-up）を適用して `MetricValue` を構築する
    - `kalos_plugin_alloc(size: u32) -> u32` — ホストが線形メモリへ書き込むためのアロケータ
    - `kalos_plugin_free(ptr: u32, size: u32)` — `kalos_plugin_alloc` で確保した領域を解放する
  - **線形メモリデータレイアウト**: ホスト⇔プラグイン間で線形メモリを介して受け渡す構造化データは Little-Endian の固定長バイナリレイアウトとする。v1 のレイアウト仕様は SPI version `kalos-metric-spi-v1` に紐づき、SPI version 変更時に破壊的変更となりうる。**v1 ノード/エッジレイアウト**: `cpg_read_node` が書き込むノードレコードは `[kind: u32, name_len: u32, name_bytes: [u8; name_len]]`（kind は CPG ノード種別の列挙値）、`cpg_read_edge` が書き込むエッジレコードは `[source_index: u32, target_index: u32, kind: u32]` とする。いずれも Little-Endian。`config_read` が書き込む値は UTF-8 文字列のバイト列（長さは戻り値で返す）とする。**ScopeId レイアウト**: 前述の「ScopeId 直列化契約」で定義する。**`kalos_plugin_evaluate` 戻り値**: WASM 値スタック上の `i64` スカラーであり、線形メモリレイアウトではない。エンコーディングは前述の「スカラー戻り値エンコーディング」を参照。上記レイアウトおよびスカラーエンコーディングは `kalos-metric-spi-v1` の一部であり、変更は SPI version の更改を伴う
  - **WASI 不使用の根拠**: 評価 SPI は pure function（`CpgSubgraph + MetricConfig -> MetricValue`）であり、OS リソースへのアクセスを必要としない。WASI を排除することで、決定論性の保証が WASM ランタイムの fuel metering のみに依存する単純なモデルとなる
- `Configuration` は `[[plugins]]` の `path` を `WorkspaceRoot` 基準で canonicalize し、`WorkspaceRoot` 外参照または `sha256` 構文不正を設定エラー（exit code 2）として扱う。Plugin Host はこの検証を通過した `plugin_manifest` だけを入力に受け取り、実行時の失敗境界と設定不正の境界を分離する
- ホストが渡すのは additive-only な `CpgSubgraph` の read-only view と `MetricConfig` だけに絞り、ネットワークやファイル書込は許可しない。Plugin Host は登録された各 `metric_id` と `MetricDefinition.level` に一致する各 `ScopeId` の組み合わせについて `kalos_plugin_evaluate(metric_id, scope)` を 1 回ずつ呼び出し、入力には `UnifiedCpg.subgraph(scope_id)` を渡す。function/module metric は該当 scope ごとに 1 回ずつ、project metric は正規形 `ScopeId(level = Project, qualified_name = "<project>", file_path = ".")` に対して 1 回だけ評価する。プラグインはロード時に stable `metric_id`, `level`, `name`, `description` を持つ `MetricDefinition` を登録し、v1 では `participation = ReportOnly`、`rule_binding = None` とする
- Plugin Host は `plugin_manifest` を `workspace_relative_path` 昇順でロードし、`metric_id` のグローバル一意性を検証する。組み込みメトリクスまたは先行ロード済みプラグインと `metric_id` が衝突したモジュールは deterministic なロード失敗として扱い、warning を出してスキップする
- **登録の原子性**: `metric_register` が衝突を検出して `-1` を返した場合、その呼び出しで要求されたメトリクスは登録されない。`kalos_plugin_init` 完了後、ホストは当該モジュールの初期化結果を以下の規則で判定する: (1) `kalos_plugin_init` が非 0 を返した場合、または (2) 初期化中のいずれかの `metric_register` 呼び出しが `-1` を返した場合、当該モジュールは **ロード失敗** として扱い、初期化中に登録されたすべての `MetricDefinition` をロールバック（取り消し）する。部分的に登録が残ることはない。これにより、1 モジュール内の `metric_id` 衝突が他のメトリクスに不整合を生じさせることを防ぐ
- プラグインファイル読込失敗、checksum 不一致、SPI version 不一致、`metric_id` 衝突、per-invocation fuel budget 超過、メモリ超過は warning + skip とし、当該プラグインのみを失敗させる。aggregate fuel budget 超過時は、`workspace_relative_path → metric_id → ScopeId` の辞書順における残りの評価を warning 付きでスキップする（pre-invocation budget check による決定論的カットオフ）。いずれも `stderr` / 構造化ログへ運用警告を出し、v1 の診断・スコア・Exit code 契約は変更しない
- `REQ-NF-003` を守るため、評価 SPI は pure function (`CpgSubgraph + MetricConfig -> MetricValue`) とし、乱数・時刻・外部 I/O を禁止する
- **WASM instance lifecycle**: Plugin Host はプラグインモジュールごとに 1 つの WASM instance を生成する。instance の生存期間は単一の `kalos check` 実行スコープに限定し、実行完了時にすべての instance を破棄する。実行間で instance を再利用しない
  - **初期化**: `plugin_manifest` 順（`workspace_relative_path` 昇順）にモジュールをロードし、`kalos_plugin_init()` を呼び出す。初期化時に `metric_register` で `MetricDefinition` を登録する
  - **評価**: 登録済み `MetricDefinition` を `workspace_relative_path → metric_id → ScopeId` の辞書順（`ScopeId` の辞書順は `(<level>, <qualified_name>, <file_path>)`、`AnalysisLevel` は `Function < Module < Project`）に従って列挙し、各組み合わせについて `kalos_plugin_evaluate` を 1 回ずつ呼び出す。**Pre-invocation budget check**: 各 `kalos_plugin_evaluate` 呼び出しの前に aggregate fuel budget の残量を検査し、残量が `0` 以下の場合は当該呼び出し以降のすべての評価を warning 付きでスキップする。これにより、aggregate budget 超過時のカットオフ位置が入力と設定から決定論的に確定する。ホストは各呼び出しの前に guest state（グローバル変数、線形メモリの内容）を初期化完了直後のスナップショットへリセットする。これにより各評価は pure function 契約（`CpgSubgraph + MetricConfig -> MetricValue`）を WASM instance レベルで満たし、呼び出し順序への依存を排除する
  - **破棄**: 全評価完了後、または per-invocation / aggregate fuel budget 超過時に instance を破棄し、線形メモリを解放する
- **線形メモリ管理**: 各 WASM instance は独立した線形メモリ空間を持つ。初期サイズは WASM モジュールの宣言に従い、`linear_memory_limit`（v1 暫定値: `64 MiB`）を上限とする。上限超過時はトラップとして扱い、当該プラグインの評価を打ち切る
- 実行リソース制限の正本は WASM fuel 単位とする。fuel は WASM 命令ごとに決定論的に消費される抽象コスト単位であり、壁時間やホスト CPU 速度に依存しない。これにより同一入力に対するプラグインの成否判定が `REQ-NF-003` の決定論性を維持する
  - **per-invocation fuel budget**: `500_000 fuel`（暫定値）。1 回の `kalos_plugin_evaluate` 呼び出しに対する上限
  - **aggregate fuel budget**: `30_000_000 fuel`（全解析、暫定値）/ `5_000_000 fuel`（diff mode、暫定値）。Metrics stage 全体でのプラグイン fuel 消費合計の上限。diff 解析から全解析へフォールバックした場合（`InvalidationPlan.fallback_to_full = true`）は、実際の実行パスに従い全解析用の budget（`30_000_000 fuel`）を適用する
  - **linear_memory_limit**: `64 MiB`（暫定値）。プラグインの線形メモリ上限
  - **参考値**: `bench-linux-x64` プロファイル（REQ-NF-001 測定条件）上で、上記 fuel budget はおおむね per-invocation ~50ms / aggregate ~3s（全解析）/ ~0.5s（diff mode）の壁時間に相当する。ただしこの対応は参考であり、**fuel 値が規範的（normative）な上限**である。壁時間は環境により変動するため契約の一部ではない
  - 上記の暫定値は PoC ベンチマークで検証し、v1 リリースまでに確定する。確定後はこの ADR を改訂する。暫定値の根拠は `REQ-NF-001`（全解析 60s）/ `REQ-NF-002`（差分解析 10s）の性能バジェットにおいて、プラグイン評価を全体の 5% 以内に収める目安から導出した
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
- fuel・メモリ上限を契約化することで `REQ-NF-001/002` との整合を説明しやすい
- v1 は report-only metric に限定することで、RuleId / score / exit code 契約を増やさずに拡張点を提供できる

### ネガティブ

- SPI version `kalos-metric-spi-v1` の保守が必要であり、破壊的変更時は新 SPI version と移行計画を ADR で決定する
- 実行性能の測定が不可欠
- プラグインは kalos バイナリとは別に配布・管理する必要がある（v1 では `.kalos.toml` の `[[plugins]]` に path と checksum を登録し、ホストが `WorkspaceRoot` 基準の `plugin_manifest` へ正規化する）
- fuel budget 超過、メモリ上限超過、aggregate fuel budget 超過、および評価戻り値の `normalized_risk` が `NaN` / `±Inf` の場合、当該プラグインは `MetricValue` を返せず、ホストは運用警告を記録したうえで当該プラグイン評価または残り評価を打ち切る（有限な範囲外値は `clamp` で補正し warning を出力する）。v1 の診断・スコア・Exit code は既存契約のまま維持する。diff mode では baseline 断片に残る当該プラグインの `MetricValue` も最終出力から除外する

### リスク

- WASM オーバーヘッドが大きい場合、初期リリースでは組み込みメトリクス中心で運用し、外部プラグインを experimental 扱いにする可能性がある
- fuel budget の暫定値（500K / 30M / 5M fuel, 64MiB）は `REQ-NF-001/002` の性能バジェットの 5% 目安から導出したが、実ワークロードで厳しすぎる、または緩すぎる可能性がある。PoC ベンチマークで検証し v1 リリースまでに確定する

## 改訂履歴

| 日付 | 変更概要 | 関連文書版 |
|---|---|---|
| 2026-03-18 | 初版承認 | architecture.md v0.1.0 |
| 2026-03-19 | SPI 設計（host/plugin exports）追加、fuel budget（per-invocation / aggregate）追加、`plugin_manifest` 正規化・SPI 互換性契約追加、`MetricDefinition` 登録契約・`metric_id` 衝突処理追加 | architecture.md v0.2.0–v0.2.11 |
| 2026-03-20 | 評価順序（`workspace_relative_path → metric_id → ScopeId` 辞書順）追加、aggregate fuel budget の diff mode 値追加 | architecture.md v0.2.12 |
| 2026-03-22 | レビュー指摘解決: WASM instance lifecycle（初期化・評価・破棄）追加、線形メモリ管理（上限・トラップ）追加、invalid-value contract（NaN/±Inf 拒否・範囲外 clamp）追加、diff→full フォールバック時の aggregate fuel budget 切替規則追加 | architecture.md v0.4.0–v0.4.3 |
| 2026-03-22 | SPI v1 ABI normative 仕様追加: ptr/len エンコーディング契約、read ヘルパー戻り値契約、v1 ノード/エッジバイナリレイアウト、登録の原子性、pre-invocation budget check | architecture.md v0.4.4 |
| 2026-03-22 | ScopeId 直列化契約追加、スカラー戻り値と線形メモリレイアウトの用語分離 | architecture.md v0.4.5 |
