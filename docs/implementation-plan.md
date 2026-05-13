# kalos 実装計画

## メタ情報

| 項目 | 内容 |
|---|---|
| 作成日 | 2026-03-28 |
| ステータス | ドラフト |
| 入力 | requirements.md v0.4.14, architecture.md v0.4.26, domain_model.md v0.4.14, design-resolution-memo.md, ADR-0001〜0005 |

## 1. 現状分析

### 1.1 リポジトリ状態

- `kalos/` は `agentic-workspace` の submodule
- `kalos/docs/` のみ存在。ソースコード・`Cargo.toml`・テスト・CI 設定は一切ない
- docs は v0.4.x まで成熟しており、4 回のレビューサイクルと 21 件の設計判断が解決済み（design-resolution-memo.md）

### 1.2 プロジェクト概要（docs からの要約）

kalos は Rust 製の CLI ツールで、ソースコードから CPG（Code Property Graph）を抽出し、情報理論・グラフ理論ベースのメトリクスでコード品質を定量評価する。

- **対象言語**: Python, TypeScript, Rust, Go
- **アーキテクチャ**: 単一 Rust バイナリのモジュラーモノリス（Ports & Adapters + Pipe-and-Filter）
- **ドメイン境界**: CPG 抽出 / メトリクス算出 / 診断 / 構成管理 / 差分解析 / レポート
- **外部依存**: CodeQL（CPG 抽出エンジン）、任意の LLM（改善提案補助）
- **設計ドライバー**: 決定論性 > 性能 > 拡張性 > 可搬性 > 可用性 > 初回利用容易性

## 2. スコープ

### In Scope（初回実装）

| # | 領域 | 対応要件 |
|---|---|---|
| 1 | リポジトリスキャフォールディング | — |
| 2 | ドメインモデル型定義 | domain_model.md §3.1–3.4 全体 |
| 3 | Configuration コンテキスト | REQ-FUNC-025–028, REQ-NF-007 |
| 4 | CLI Shell（clap ベース） | REQ-FUNC-018, 022, 023, 030 |
| 5 | Application Pipeline | REQ-FUNC-018, 022–024, 034 |
| 6 | CPG Extraction（ExtractorPort + CodeQL Adapter） | REQ-FUNC-001–007, 031 |
| 7 | Metrics コンテキスト（組み込み 10 メトリクス + スコアリング） | REQ-FUNC-008–011 |
| 8 | Diagnostics コンテキスト（ルール評価 + パターン検出 + テンプレート提案） | REQ-FUNC-013–017, 026, 029 |
| 9 | Reporting コンテキスト（human / JSON / SARIF） | REQ-FUNC-019–021, 024, 033 |
| 10 | Diff Analysis（Git Diff Adapter + Baseline Cache + Impact Analysis） | REQ-FUNC-034, REQ-NF-002 |
| 11 | Plugin Host（WASM メトリクスプラグイン） | REQ-FUNC-012, REQ-NF-006 |
| 12 | LLM Adapter | REQ-FUNC-015, REQ-NF-008–010 |
| 13 | Managed Tool Cache Adapter（CodeQL bundle bootstrap） | REQ-FUNC-031–032, REQ-NF-009–010 |
| 14 | CI/CD 統合（GitHub Actions Action） | REQ-FUNC-032–033 |

### Out of Scope

| 項目 | 除外理由 |
|---|---|
| IDE 統合（LSP） | 要件定義で将来拡張と明記 |
| GUI/ダッシュボード | 同上 |
| 自動修正コード生成 | 同上 |
| WASM Plugin SDK / 配布パッケージ | PoC 項目 #2（v1.1 設計対象） |
| 組み込み scored metric の追加（v1.1） | REQ-NF-006 で v1.1 以降と明記 |
| CodeQL 代替エンジン | PoC 項目 #1 |

## 3. タスク分解

### Wave 0: リポジトリスキャフォールディング

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 0-1 | `Cargo.toml` ワークスペース構成の作成 | `cargo check` が成功する | なし | 高 |
| 0-2 | モジュール構成の作成（architecture.md §4.3 準拠） | `src/` 以下に cli/, application/, domains/, ports/, adapters/, platform/ が存在し、各 `mod.rs` でコンパイルが通る | 0-1 | 高 |
| 0-3 | 基本依存クレートの追加 | `clap`, `serde`, `toml`, `petgraph`, `tracing` が `Cargo.toml` に記載され `cargo check` が通る | 0-1 | 高 |
| 0-4 | CI スケルトン（GitHub Actions） | `cargo check`, `cargo test`, `cargo clippy` が CI で実行される | 0-1 | 中 |
| 0-5 | `.kalos.toml` サンプルと `kalos init` スタブ | `kalos init` がデフォルト設定を出力する（REQ-FUNC-030 の最小形） | 0-2 | 中 |

### Wave 1: ドメインモデル型定義 + Configuration

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 1-1 | CPG 抽出コンテキストの型定義 | `SourceAnalysis`, `UnifiedCpg`, `CpgNode`, `CpgEdge`, `NodeKind`, `EdgeKind`, `Language`, `SourceFile`, `SuppressionComment`, `AnalysisWarning` が domain_model.md §3.1 に準拠して定義され、`cargo test` が通る | 0-2 | 高 |
| 1-2 | メトリクス算出コンテキストの型定義 | `AnalysisMetrics`, `ScopeMetrics`, `MetricDefinition` trait, `MetricValue`, `OverallScore`, `ScopeId`, `ScoreWeights`, `AnalysisLevel`, `MetricParticipation` が domain_model.md §3.2 に準拠して定義される | 0-2 | 高 |
| 1-3 | 診断コンテキストの型定義 | `DiagnosticReport`, `Diagnostic`, `MetricRule`, `PatternRule`, `MetricObservation`, `PatternEvidence`, `RuleConfig`, `InlineSuppression`, `LlmSuggestionBundle` が domain_model.md §3.3 に準拠して定義される | 0-2 | 高 |
| 1-4 | 差分解析コンテキストの型定義 | `DiffBaseline`, `BaselineFingerprint`, `AffectedScopeSet`, `InvalidationPlan`, `DependencyIndexManifest`, `ScopeDiagnosticSnapshot` が domain_model.md §3.4 に準拠して定義される | 0-2 | 高 |
| 1-5 | ポートトレイト定義 | `ExtractorPort`, `DependencyResolverPort`, `LlmPort`, `PluginPort`, `CachePort`, `ToolCachePort`, `DiffSourcePort` の trait が ports/ に定義される | 1-1, 1-2, 1-3, 1-4 | 高 |
| 1-6 | Configuration コンテキスト実装 | `.kalos.toml` パース、`WorkspaceRoot` 解決、`ProjectConfig` 生成、優先順位マージ（CLI > ファイル > デフォルト）、`analysis_targets` 正規化が動作し、設定バリデーション（weights > 0, threshold ∈ [0,1], sha256 形式）で不正値を exit code 2 で拒否する。テスト付き | 1-5 | 高 |
| 1-7 | CLI Shell 実装（clap） | `kalos check [<path>...] --format --level --evaluation-profile --config --exclude --severity --diff --llm --strict` と `kalos init` の引数パースが動作する。位置引数省略時のデフォルト `.` 処理を含む | 1-6 | 高 |

### Wave 2: メトリクスエンジン

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 2-1 | 関数レベルメトリクス実装（M-F001〜M-F004） | 4 メトリクスが REQ-FUNC-008 の数式に従い `CpgSubgraph` から算出される。同一入力で `raw_value` と `normalized_risk` がビット単位一致（round-half-up 第 6 位）。テスト付き | 1-1, 1-2 | 高 |
| 2-2 | モジュールレベルメトリクス実装（M-M001〜M-M003） | 3 メトリクスが REQ-FUNC-009 の数式に従い算出される。テスト付き | 1-1, 1-2 | 高 |
| 2-3 | プロジェクトレベルメトリクス実装（M-P001〜M-P003） | 3 メトリクスが REQ-FUNC-010 の数式に従い算出される。テスト付き | 1-1, 1-2 | 高 |
| 2-4 | スコアリングエンジン（OverallScore 算出） | `scope_risk` → `level_risk` → `overall_risk` → `overall_score` の集約が REQ-FUNC-011 ステップ 1–9 に完全準拠。re-normalization と empty-level redistribution を含む。`enabled = false` のメトリクスを `scope_risk` 母集団から除外する。テスト付き | 2-1, 2-2, 2-3 | 高 |

### Wave 3: 診断エンジン

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 3-1 | MetricRule 評価（閾値比較 + overflow_ratio + 重大度付与） | `normalized_risk > threshold` で診断生成。`overflow_ratio` 算出と 3 段階重大度（info/warning/error）のデフォルト判定。`rules.<RuleId>.severity` による上書き。テスト付き | 2-4 | 高 |
| 3-2 | PatternRule 検出（KAL-PAT001, PAT002, PAT003） | God Unit（PAT001: 4 言語の `public_member_count` 計数規則準拠）、Feature Envy（PAT002: foreign/local アクセス比率）、Circular Dependency（PAT003: SCC 検出）が検出される。テスト付き | 2-4, 1-1 | 高 |
| 3-3 | テンプレート改善提案生成 | 各ルールに対して `TemplateSuggestion`（explanation + optional code_example）が生成される | 3-1, 3-2 | 中 |
| 3-4 | インライン抑制（kalos-ignore） | `SuppressionComment` に基づき、一致する行/スコープの診断を抑制する。cross-scope 診断の synthetic 代表位置には適用しない。テスト付き | 3-1, 3-2 | 中 |

### Wave 4: レポーティング

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 4-1 | Application Pipeline オーケストレーション | non-diff フローで Configuration → CPG Extraction → Metrics → Diagnostics → Report の呼び出しチェーンが動作する。`DiagnosticReport` の assemble（summary materialization 含む）と exit code 判定。`--strict` セマンティクス | 1-7, 2-4, 3-1 | 高 |
| 4-2 | Human-readable 出力 | REQ-FUNC-019 の出力形式に準拠。メトリクス診断/パターン診断の出力分岐。色分け。`--level` による射影。Summary 表示 | 4-1 | 高 |
| 4-3 | JSON 出力 | REQ-FUNC-020 の必須フィールド（`schema_version`, `analysis_targets`, `scores`, `metrics`, `diagnostics`, `diagnostics_scope`, `summary`, `summary_scope`, `tool_version`）を含む。`--level` 射影。`participation` フィールド付き。テスト付き | 4-1 | 高 |
| 4-4 | SARIF 出力 | SARIF 2.1.0 準拠。REQ-FUNC-021 の写像方針（rules[], ruleId, level, location, message, properties.kalos）に完全準拠。テスト付き | 4-1 | 中 |
| 4-5 | `--level` 射影ロジック | Reporting が `ReportViewOptions.requested_level` に基づき、非対象階層のメトリクス・診断・スコアを must exclude する。`summary_scope` の正確な選択 | 4-2, 4-3, 4-4 | 高 |

### Wave 5: CPG 抽出（CodeQL Adapter）

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 5-1 | Managed Tool Cache Adapter | CodeQL bundle の managed manifest に基づく bootstrap（固定バージョン + SHA-256 検証）、キャッシュ解決、オフライン時のエラーメッセージ。テスト付き | 0-3 | 高 |
| 5-2 | CodeQL Adapter（ExtractorPort 実装） | CodeQL CLI をサブプロセスとして呼び出し、DB 作成 → クエリ実行 → 結果取得を行う。引数配列で実行（シェル展開しない）。4 言語対応。テスト付き | 5-1, 1-5 | 高 |
| 5-3 | UnifiedCpg 正規化 | CodeQL 出力から `UnifiedCpg` への変換。共通構造 + 言語固有 `LanguageExtension`。ファイル列挙順の正規化（絶対パス昇順）。`SourceAnalysis` の組み立て | 5-2, 1-1 | 高 |
| 5-4 | ファイル収集・除外解決 | ディレクトリ再帰走査、拡張子フィルタ、`.gitignore` + 設定ファイル + CLI `--exclude` の和集合除外。テスト付き | 1-6 | 高 |
| 5-5 | 外部シンボル解決（Dependency Symbol Resolver Port） | Cargo.toml / package.json / go.mod / requirements.txt からの公開 API シグネチャ取得。`ExternalSymbol` ノード統合。解決失敗は warning。ネットワーク不要 | 5-3, 1-5 | 中 |

### Wave 6: 差分解析

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 6-1 | Git Diff Adapter | `base-ref` 解決、変更ファイル列挙、`base_snapshot_hash` 取得。テスト付き | 1-5 | 中 |
| 6-2 | Baseline Cache Adapter | `DiffBaseline` の保存/読み戻し（原子的書き込み）。`BaselineFingerprint` 完全一致判定。`$KALOS_CACHE_DIR` 解決。テスト付き | 1-4 | 中 |
| 6-3 | Impact Analysis Service | merged dependency graph 生成（baseline 辺 + 差分 CPG 辺の置換）、逆推移的閉包で `AffectedScopeSet` 算出、`InvalidationPlan` 生成。Project scope 常時再計算。フォールバック条件。テスト付き | 6-1, 6-2, 1-4 | 中 |
| 6-4 | diff フローの Application Pipeline 統合 | `--diff <base-ref>` 指定時に diff フローが動作。subset `analysis_targets` 判定。フォールバック。`diagnostics_scope` / `summary_scope` の正確な設定。テスト付き | 6-3, 4-1 | 中 |

### Wave 7: プラグインシステム

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 7-1 | Plugin Host（WASM ローダー） | `plugin_manifest` から WASM モジュールをロード。`workspace_relative_path` 昇順。checksum 検証。SPI version 検査。`metric_id` 衝突検出と登録の原子性。テスト付き | 1-5 | 中 |
| 7-2 | SPI v1 ABI 実装 | ADR-0004 準拠の host exports / guest exports。read ヘルパー、ptr/len エンコーディング、ScopeId 直列化、スカラー戻り値。テスト付き | 7-1 | 中 |
| 7-3 | Fuel/memory budget enforcement | per-invocation fuel budget (500K fuel)、aggregate fuel budget (30M/5M fuel)、linear_memory_limit (64 MiB)。pre-invocation budget check。超過時の warning + skip。テスト付き | 7-2 | 中 |
| 7-4 | diff mode プラグイン再利用ゲート | 現在の実行で正常ロード・評価されたプラグインのみ baseline `MetricValue` を再利用。失敗/スキップ分は除外 | 7-3, 6-3 | 中 |

### Wave 8: LLM 統合

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 8-1 | LLM Adapter（HTTP） | `KALOS_LLM_PROVIDER` / `KALOS_LLM_ENDPOINT_URL` / `KALOS_LLM_API_KEY` による設定。connect timeout 3s / overall timeout 30s。429 リトライ（1 回）、5xx skip。URL 秘匿化ログ。テスト付き | 1-5 | 低 |
| 8-2 | LlmEnrichmentRequest 組み立て | `Diagnostic` + `SourceAnalysis` から allowlist 済みフィールドを組み立て。`source_excerpt` / `cpg_excerpt` 排他。代表ファイル言語解決。multi-file 診断のスキップ | 8-1, 4-1 | 低 |
| 8-3 | Aggregate sidecar budget | 120s 壁時間上限。cumulative accounting。上限到達後の skip + warning | 8-2 | 低 |
| 8-4 | Preflight failure | `--llm` 時の `KALOS_LLM_API_KEY` 未設定、URL 不正、provider 不正で exit code 2 | 8-1 | 低 |

### Wave 9: CI/CD・配布

| # | タスク | 受入基準 | 依存 | 優先度 |
|---|---|---|---|---|
| 9-1 | クロスプラットフォームビルド | Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64 のプリビルドバイナリが GitHub Actions で生成される | 0-4 | 中 |
| 9-2 | GitHub Actions 公式 Action | `uses: kalos/action@v1` でバイナリ取得 + bundle prewarm + baseline cache restore/save + 解析実行が動作する | 9-1, 5-1 | 中 |
| 9-3 | 決定論性適合度テスト | 固定コーパスに対する 10 回実行で JSON ハッシュが全一致する CI テスト | 4-3 | 中 |
| 9-4 | 性能ベンチマーク CI | 10k LOC コーパスで全解析 60s / diff 10s の p95 を計測する nightly CI | 5-2, 6-4 | 中 |

## 4. 実行戦略

### 4.1 並行実行可能なグループ

```
Wave 0                            （スキャフォールディング）
  ↓
Wave 1                            （型定義 + Configuration + CLI）
  ↓
Wave 2 ─────────── Wave 5-4       （メトリクス ‖ ファイル収集）
  ↓                    ↓
Wave 3           Wave 5-1,5-2,5-3 （診断 ‖ CodeQL Adapter）
  ↓                    ↓
Wave 4 ←──────────────┘           （レポーティング — 両方の出力を統合）
  ↓
Wave 6 ─── Wave 7 ─── Wave 8     （差分解析 ‖ プラグイン ‖ LLM — 並行可能）
  ↓
Wave 9                            （CI/CD・配布）
```

### 4.2 クリティカルパス

```
Wave 0 → Wave 1 → Wave 2 → Wave 3 → Wave 4 → Wave 6 → Wave 9
```

Wave 5（CodeQL Adapter）は Wave 4 の完了前に並行開始可能だが、E2E テストの実行には Wave 4 が必要。

### 4.3 推奨ロール割り当て

| Wave | 推奨ロール | 理由 |
|---|---|---|
| 0 | code-writer + scaffolder スキル | 標準的なプロジェクトセットアップ |
| 1 | implementer（設計判断含む） | ドメインモデルの Rust 型への変換は設計判断を伴う |
| 2, 3 | code-writer × 2（並行可能） | 数式が確定済みで実装に集中できる |
| 4 | implementer | 3 つの出力形式の整合性保証 |
| 5 | implementer | CodeQL 連携は外部プロセス制御の設計判断あり |
| 6, 7, 8 | code-writer × 3（並行可能） | 各ドメインが独立 |
| 9 | code-writer | CI/CD パイプライン構築 |

### 4.4 テスト分離戦略

パイプライン依存チェーン `CPG → Metrics → Diagnostics → Report` のうち、Wave 2–4 は **CodeQL Adapter なしでテスト可能** にする:

1. **Mock ExtractorPort**: テスト用にハードコードされた `UnifiedCpg` を返す `MockExtractor` を用意
2. **テスト用 CPG ビルダー**: `CpgBuilder` ヘルパーで関数・モジュール・エッジを宣言的に構築
3. **E2E テスト**: Wave 5 完了後に実際の CodeQL 出力を使ったインテグレーションテストを追加

これにより、Wave 2–4 は CodeQL のインストールや実行なしに開発・テストを進められる。

## 5. リスクとブロッカー

| # | リスク | 影響 | 緩和策 |
|---|---|---|---|
| R-1 | CodeQL bundle の抽出時間が 60s 性能目標を超過する | REQ-NF-001 未達 | PoC 項目 #4 として早期ベンチマーク。超過時は代替エンジン（Joern 等）を ADR-0002 に基づき評価 |
| R-2 | CodeQL の CPG 出力形式が言語間で大きく異なる | UnifiedCpg 正規化の複雑化 | Wave 5-3 を早期に 1 言語で PoC し、正規化パターンを確立してから 4 言語に展開 |
| R-3 | WASM プラグイン SPI の ABI 安定性確保 | プラグイン互換性 | ADR-0004 で ABI が詳細定義済み。SPI v1 のみサポートで固定 |
| R-4 | 決定論性の浮動小数点丸め差異 | REQ-NF-003 違反 | round-half-up 第 6 位のヘルパー関数を共通化し、全集約パスで使用。決定論性適合度テスト（9-3）で回帰検知 |
| R-5 | 差分解析の merged dependency graph の正確性 | 誤った影響範囲判定 | design-resolution-memo.md §3 で契約確定済み。手厚いユニットテストとフォールバック（全解析へ）で安全側に倒す |
| R-6 | 4 言語の外部シンボル解決の実装コスト | REQ-FUNC-007 が Should 優先度だが工数大 | PoC 項目 #3 として追跡。v1 では解決失敗 = warning で許容 |

## 6. 初期実装の最小セット（MVP）

コア価値（「既存リンターが検出しない構造的改善点の指摘」）を最速で検証するための最小セット:

1. **Wave 0**: スキャフォールディング
2. **Wave 1**: ドメイン型 + Configuration + CLI
3. **Wave 2**: メトリクスエンジン（10 メトリクス + スコアリング）
4. **Wave 3**: 診断エンジン（ルール評価 + パターン検出 + テンプレート提案）
5. **Wave 4**: Human + JSON 出力 + Exit code
6. **Wave 5**: CodeQL Adapter（1 言語: Rust から開始）

MVP では差分解析（Wave 6）、プラグイン（Wave 7）、LLM（Wave 8）を後回しにし、単一言語での end-to-end パイプライン動作を優先する。

## 7. 妥当性チェック

- [x] 全 In Scope 要件に対応するタスクが定義されている
- [x] 全リーフタスクに検証可能な受入基準が設定されている
- [x] 依存関係に循環がない
- [x] クリティカルパスが明示されている
- [x] 優先順位の根拠が記録されている（成功基準の優先順位 + 技術的依存関係）
