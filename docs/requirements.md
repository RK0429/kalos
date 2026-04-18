# kalos 要件定義書

## メタ情報

| 項目 | 内容 |
|---|---|
| バージョン | 0.4.14 |
| 最終更新日 | 2026-03-27 |
| ステータス | ドラフト |
| 作成者 | Claude（requirements-definer スキル） |
| レビュー者 | Codex（対象: ~v0.2.12, 2026-03-20。指摘解決は v0.3.0–v0.4.1 で適用。詳細は [design-resolution-memo.md](./design-resolution-memo.md)） |

## 1. プロジェクト概要

### 1.1 背景・動機

AIエージェントによるコーディングの発達に伴い、生成されるコードの量が増大する一方、人力コードレビューがボトルネック化している。既存のリンター（Clippy, ESLint, Ruff等）は構文エラーやスタイル違反の検出に優れるが、情報理論やグラフ理論に基づくコード構造の定量評価は行わない。コードの「美しさ」——凝集度、結合度、情報エントロピー、依存グラフの構造特性——を機械的かつ再現可能に評価するツールが求められている。

### 1.2 目的

ソースコードからコードプロパティグラフ（CPG）を抽出し、情報理論・グラフ理論に基づくメトリクスで「ソフトウェア実装の美しさ」を定量評価するCLIツール kalos を提供する。kalos は関数・モジュール・プロジェクトの各階層でメトリクスを算出し、閾値違反に対して具体的な改善提案を出力する。

### 1.3 対象ユーザー

- **個人開発者**: 自分のコードの構造的品質を自己評価し、改善点を把握する
- **開発チーム**: チーム内の品質基準を統一し、CI/CDパイプラインの品質ゲートとして運用する

前提として、ユーザーはCLIツールの基本的な操作に習熟しており、コード品質の概念（結合度・凝集度等）について基礎的な理解を持つ。

### 1.4 スコープ

**スコープ内（初回リリース）**:

| 機能領域 | 説明 |
|---|---|
| CPG抽出 | Python, TypeScript, Rust, Go のソースからCPGを生成 |
| メトリクス算出 | 関数・モジュール・プロジェクトの各階層でメトリクスを算出 |
| 診断・改善提案 | 閾値違反の検出と具体的な改善提案の生成 |
| CLIインターフェース | 解析実行、結果表示、設定管理 |
| 設定・ルール管理 | プロジェクトごとのルール・閾値カスタマイズ |
| CI/CD統合 | GitHub Actions対応、exit code制御、機械可読出力 |

**スコープ外（将来的な機能拡張として検討）**:

| 機能領域 | 説明 |
|---|---|
| IDE統合 | LSPサーバー、エディタプラグイン |
| GUI/ダッシュボード | Web UIによる可視化・トレンド表示 |
| 自動修正 | 検出した問題の自動修正コード生成 |

### 1.5 制約条件

| 制約 | 内容 |
|---|---|
| 実装言語 | Rust |
| CPG抽出エンジン | CodeQLを前提とする。CFG/DFG を含む CPG 抽出能力を持つ代替（Joern 等）があれば設計フェーズで比較検討する。Tree-sitter は構文解析基盤であり CPG 抽出エンジンのパーサー層として補完的に利用し得るが、単体では CFG/DFG を生成しないため CPG 抽出エンジンの代替候補ではない |
| 対応OS | Linux (x86_64, aarch64), macOS (x86_64, aarch64), Windows (x86_64) |
| CI/CD基盤 | GitHub Actionsを主要ユースケースとして想定（特定基盤に限定しない） |

### 1.6 成功基準

以下の優先順位で評価する。

1. **既存リンターの超越**: 既存リンター（Clippy, ESLint, Ruff, golangci-lint等）が検出しない構造的改善点を指摘できる
2. **改善提案の具体性**: 「こう直すべき」レベルの具体的な改善提案を提示できる
3. **評価の再現性**: 同一コード・同一設定に対して常に同一のスコアと診断を返す（決定論的評価）
4. **CI/CD実用性**: CI/CDパイプラインで実用的な実行速度を達成する

## 2. 用語集

| 用語 | 定義 |
|---|---|
| CPG（コードプロパティグラフ） | AST（抽象構文木）、CFG（制御フローグラフ）、DFG（データフローグラフ）を統合したグラフ構造。ソースコードの構文・制御フロー・データフローを単一のグラフで表現する |
| メトリクス | CPGから算出される定量的な評価指標。情報理論（エントロピー等）やグラフ理論（結合度、モジュラリティ等）に基づく |
| 診断 | メトリクスの閾値違反や構造的パターンの検出結果。ファイルパス・行範囲・ルールID・重大度・メッセージを含む |
| 改善提案 | 診断に対する具体的な修正方針のテキスト。何が問題か、なぜ問題か、どう改善すべきかを記述する |
| ルール | 特定のメトリクスまたはパターンに対する診断生成規則。一意のルールID（`KAL-F001`, `KAL-M001`, `KAL-P001`, `KAL-PAT001` 形式）で識別され、閾値・重大度・提案テンプレートなどの詳細契約は rule 種別ごとに決まる |
| 総合スコア | 各階層の正規化リスク値（0.0〜1.0, 高いほど悪い）を重み付き集約し、`100 * (1 - overall_risk)` で算出する品質スコア |
| ワークスペースルート | kalos が解析の基準ディレクトリとして解決するルート。`--config <path>` 指定時はその `.kalos.toml` の親ディレクトリを採用し、未指定時はカレントディレクトリから親方向に探索して最初に見つかった `.kalos.toml` の親ディレクトリを優先し、見つからない場合は最初に見つかった `.git` の親ディレクトリ、どちらもなければ実行時カレントディレクトリを採用する |
| ワークスペース相対パス | ワークスペースルート基準で正規化されたパス。内部 `FilePath`、`plugin_manifest` のプラグイン参照、LLM sidecar の `workspace_relative_path` はこの形式を用いる |
| 統一CPG表現 | 4言語のCPGを言語非依存な共通構造と言語固有の拡張ノードで表現する内部データ構造 |
| CodeQL | GitHub が開発するコード解析エンジン。ソースコードをデータベース化し、クエリ言語でコードプロパティを検索・抽出できる |
| SARIF | Static Analysis Results Interchange Format。静的解析ツールの結果を表現するJSON形式の標準規格（OASIS標準） |
| Configuration（構成管理コンテキスト） | `.kalos.toml` の読み込み・検証・正規化を担う境界づけられたコンテキスト。集約ルートは `ProjectConfig` であり、`ProjectConfig.resolve()` が設定解決を行う。詳細は architecture.md §4.1 参照 |
| CLI Shell | CLI 引数の解釈・バリデーションを担うコンポーネント。`--level`, `--diff`, `--format` 等のオプションを解釈し、Application Pipeline へ渡す。詳細は architecture.md §4.1 参照 |
| Application Pipeline | パイプラインオーケストレーションを担うコンポーネント。diff/non-diff モード選択、`DiagnosticReport` の組み立て（summary materialization を含む）、exit code 判定を行う。詳細は architecture.md §4.1 参照 |
| Plugin Host | WASM プラグインのロード・検証・実行を担うコンポーネント。`plugin_manifest` に基づくプラグインの決定論的ロードとサンドボックス実行を管理する。詳細は architecture.md §4.1 参照 |
| DiagnosticReport | 診断コンテキストの集約ルート。診断一覧・一覧の完全性（`diagnostics_scope`）・重大度別件数サマリー（`summary`）・サマリー母集団（`summary_scope`）を束ねる。Exit code はフィールドとして保持せず `determine_exit_code(strict)` で都度導出する。レポートコンテキストでは `AnalysisMetrics`・`ReportMetadata` と組み合わせて human/JSON/SARIF 形式への写像元となる。詳細は domain_model.md §3.3、architecture.md §4.1 参照 |
| SourceAnalysis | CPG 抽出コンテキストの集約ルート。統一 CPG・ソースファイルメタデータ・抑制コメント・解析警告を束ねる。詳細は domain_model.md §3.1 参照 |
| ScopeId | メトリクス算出対象を一意に識別する値。階層（Function / Module / Project）・修飾名・ファイルパスで構成する。詳細は domain_model.md §3.2 参照 |
| MetricDefinition | メトリクスの計算方法を定義するエンティティ。組み込みとプラグインが同一インターフェースを実装する。詳細は domain_model.md §3.2 参照 |
| MetricValue | 算出された生値（`raw_value`）と 0〜1 の正規化リスク値（`normalized_risk`）のペア。詳細は domain_model.md §3.2 参照 |
| BaselineFingerprint | 差分解析ベースラインの互換性を判定するための構成要素ハッシュの組。`workspace_root_hash`、`base_snapshot_hash`、`config_hash` 等を含む。詳細は domain_model.md §3.4 参照 |

## 3. 機能要件

### 3.1 CPG抽出

#### REQ-FUNC-001: Python ソースからのCPG生成

- **説明**: Python ソースファイルを解析し、統一CPG表現に変換する
- **入力**: Python ソースファイル（`.py`）
- **処理**: CodeQL（または選定エンジン）によるAST・CFG・DFG抽出 → 統一CPG表現への変換（共通構造 + Python固有拡張ノード）
- **出力**: 統一CPGデータ構造
- **前提条件**: ソースが構文的に有効であること
- **例外**: 構文エラーを含むファイルはスキップし、警告を出力する。他ファイルの解析は継続する
- **受け入れ基準**:
  - Given 有効なPythonソース, When CPG生成を実行, Then 関数・クラス・モジュール間の呼び出し関係・データフロー・制御フローがノード/エッジとして抽出される
  - Given 構文エラーを含むPythonファイル, When CPG生成を実行, Then 当該ファイルをスキップし警告を出力し、他ファイルの解析は継続する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-002: TypeScript ソースからのCPG生成

- **説明**: TypeScript ソースファイルを解析し、統一CPG表現に変換する
- **入力**: TypeScript ソースファイル（`.ts`, `.tsx`）
- **処理**: REQ-FUNC-001 と同様の処理フロー。TypeScript固有の型情報を拡張ノードとして保持する
- **出力**: 統一CPGデータ構造
- **前提条件**: ソースが構文的に有効であること
- **例外**: REQ-FUNC-001 と同様
- **受け入れ基準**:
  - Given 有効なTypeScriptソース, When CPG生成を実行, Then 関数・クラス・モジュール間の呼び出し関係・データフロー・制御フローがノード/エッジとして抽出される
  - Given 構文エラーを含むTypeScriptファイル, When CPG生成を実行, Then 当該ファイルをスキップし警告を出力し、他ファイルの解析は継続する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-003: Rust ソースからのCPG生成

- **説明**: Rust ソースファイルを解析し、統一CPG表現に変換する。所有権・借用・ライフタイム等のRust固有概念を拡張ノードおよび semantic edge metadata として保持する
- **入力**: Rust ソースファイル（`.rs`）
- **処理**: REQ-FUNC-001 と同様の処理フロー。Rust固有の所有権モデルを拡張ノードおよび semantic edge metadata として保持する
- **出力**: 統一CPGデータ構造
- **前提条件**: ソースが構文的に有効であること
- **例外**: REQ-FUNC-001 と同様
- **受け入れ基準**:
  - Given 有効なRustソース, When CPG生成を実行, Then 関数・モジュール間の呼び出し関係・データフロー・制御フロー・所有権関係がノード/エッジとして抽出される
  - Given 構文エラーを含むRustファイル, When CPG生成を実行, Then 当該ファイルをスキップし警告を出力し、他ファイルの解析は継続する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-004: Go ソースからのCPG生成

- **説明**: Go ソースファイルを解析し、統一CPG表現に変換する。goroutine・channel等のGo固有概念を拡張ノードとして保持する
- **入力**: Go ソースファイル（`.go`）
- **処理**: REQ-FUNC-001 と同様の処理フロー。Go固有の並行処理モデルを拡張ノードとして保持する
- **出力**: 統一CPGデータ構造
- **前提条件**: ソースが構文的に有効であること
- **例外**: REQ-FUNC-001 と同様
- **受け入れ基準**:
  - Given 有効なGoソース, When CPG生成を実行, Then 関数・パッケージ間の呼び出し関係・データフロー・制御フローがノード/エッジとして抽出される
  - Given 構文エラーを含むGoファイル, When CPG生成を実行, Then 当該ファイルをスキップし警告を出力し、他ファイルの解析は継続する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-005: 複数ファイル/ディレクトリの一括解析

- **説明**: ファイルパスまたはディレクトリパスのリストを受け取り、配下の対応言語ファイルすべてを解析対象とする
- **入力**: ファイルパス/ディレクトリパスのリスト
- **処理**: ディレクトリは再帰的に走査し、対応拡張子（`.py`, `.ts`, `.tsx`, `.rs`, `.go`）のファイルを収集する。複数言語が混在する場合、各ファイルを対応する言語パーサーで処理する
- **出力**: 全対象ファイルの統一CPG（プロジェクト全体のグラフとして結合）
- **受け入れ基準**:
  - Given ディレクトリパス, When 解析実行, Then 配下の対応言語ファイルすべてがCPG生成対象になる
  - Given 複数言語が混在するディレクトリ, When 解析実行, Then 各ファイルが対応する言語パーサーで処理される
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-006: 解析対象の除外パターン指定

- **説明**: glob パターンにより解析対象ファイルの除外を制御する。v1 では包含側の allowlist（`--include`）は提供しない
- **入力**: CLI 引数 `--exclude <pattern>`（繰り返し可）、または設定ファイル `[general].exclude`（glob パターンの配列）
- **処理**: 指定パターンにマッチするファイルを解析対象から除外する。実効除外集合は `.gitignore` の既定除外、設定ファイル `[general].exclude`（配列）、CLI `--exclude`（繰り返し指定）を正規化した和集合とし、CLI は下位設定を置換せず追加する。v1 では negation パターンによる除外解除は提供しない
- **受け入れ基準**:
  - Given `--exclude "vendor/**"` 指定, When 解析実行, Then vendor配下のファイルは解析対象から除外される
  - Given `.gitignore` が存在し除外指定なし, When 解析実行, Then `.gitignore` のパターンに該当するファイルは除外される
  - Given `.gitignore`, 設定ファイル, CLI の3経路で除外パターンが指定, When 解析実行, Then 実効除外集合はそれらの和集合として解釈される
- **優先度**: Must
- **出典**: ユーザー確認済み（`.gitignore` デフォルト除外はエージェント推測→ユーザー確認済み）

#### REQ-FUNC-007: 外部依存の型情報・シグネチャ解決

- **説明**: 外部ライブラリの型情報・関数シグネチャを解決し、CPGのモジュール間エッジの精度を確保する
- **入力**: プロジェクトの依存関係定義（v1 対象: `Cargo.toml`, `package.json`, `go.mod`, `requirements.txt` / `pyproject.toml`）、対応する lockfile、およびローカルに利用可能な型スタブ/メタデータ cache
- **処理**:
  - 各言語の dependency symbol resolver adapter が、依存関係定義・lockfile・ローカルに利用可能な型スタブ/メタデータ cache を利用して外部依存の公開API（関数シグネチャ、型定義）を取得する
  - 解析実行中の追加ネットワーク通信は行わず、取得した情報をCPGの `ExternalSymbol` ノードとして統合する
- **例外**: 型情報が取得できない依存については、解決失敗として警告を出力する。メトリクス算出時には解決済みの依存のみで精度の範囲内の評価を行う
- **受け入れ基準**:
  - Given `Cargo.toml` に記載された外部クレート, When CPG生成を実行, Then 当該クレートの公開関数シグネチャがCPGの外部ノードとして含まれる
  - Given `package.json` + lockfile に記載された npm パッケージ, When CPG生成を実行, Then 当該パッケージの公開型定義がCPGの外部ノードとして含まれる
  - Given `go.mod` に記載された外部モジュール, When CPG生成を実行, Then 当該モジュールの exported シンボルがCPGの外部ノードとして含まれる
  - Given `requirements.txt` / `pyproject.toml` に記載された Python パッケージ, When CPG生成を実行, Then 型スタブが利用可能な場合は公開シグネチャがCPGの外部ノードとして含まれる
  - Given 型情報が取得できない依存, When CPG生成を実行, Then 解決失敗の警告を出力する
  - Given ネットワーク未接続かつ依存定義・lockfile・必要なローカル metadata が存在, When CPG生成を実行, Then 追加の外部通信なしで外部シンボル解決が試行される
- **優先度**: Should
- **出典**: ユーザー明示（「unknownは投げやりなので、きちんと解決すること」）
- **関連要件**: REQ-FUNC-001〜004

### 3.2 メトリクス算出

v1 では、すべてのメトリクスを `raw_value` と `normalized_risk` の組で保持する。`normalized_risk` は `0.0〜1.0` の閉区間に正規化されたリスク値であり、`0.0` が最良、`1.0` が最悪を表す。`H` は底 2 の Shannon entropy、`clamp(x, 0, 1)` は 0 未満を 0、1 超を 1 に丸める操作とする。`raw_value` または `normalized_risk` の算出結果が `NaN` または `Inf` の場合は評価失敗として扱い、warning を出力し当該メトリクスの `MetricValue` を生成しない。`normalized_risk` が有限だが `[0.0, 1.0]` 範囲外の場合は warning を出力したうえで `[0.0, 1.0]` にクランプし、クランプ後の値に対して round-half-up する。`raw_value`, `normalized_risk`, `scope_risk`, `level_risk`, `overall_risk`, `overflow_ratio` は、それぞれ算出直後に小数第 6 位で round-half-up し、その丸め済み値をキャッシュ・比較・外部出力に用いる。

> **校正注記**: v1 のデフォルト閾値・重大度境界・パターン検出カットオフは、一般的なソフトウェア品質メトリクスの知見を参考にした設計時判断による暫定値であり、特定の実証研究に裏付けられたものではない（設計判断の経緯は design-resolution-memo.md §8 を参照）。実プロジェクトでのフィードバックに基づき v2 以降で校正を予定する。見直し条件: (1) 偽陽性率が 30% を超える、(2) 偽陰性率が 20% を超える、(3) ユーザーフィードバックで特定の閾値に苦情が集中する。これらの見直し閾値自体も同設計判断に基づく暫定値である。

#### REQ-FUNC-008: 関数レベルメトリクスの算出

- **説明**: CPG の関数サブグラフに対して、v1 で固定した関数レベルメトリクスを算出する
- **入力**: 統一CPGの関数サブグラフ
- **処理**: v1 では以下の 4 メトリクスを必ず算出する

  | MetricId | RuleId | 名称 | `raw_value` | `normalized_risk` | デフォルト閾値 |
  |---|---|---|---|---|---|
  | `M-F001` | `KAL-F001` | CFG分岐エントロピーリスク | `avg_{b∈B}(log2(out_degree(b)) / log2(4))`。`B` は `out_degree > 1` の分岐ノード集合。`B` が空なら `0` | `clamp(raw_value, 0, 1)` | `0.55` |
  | `M-F002` | `KAL-F002` | サイクロマティック複雑度リスク | `M = E - N + 2`（関数 CFG の McCabe complexity） | `clamp((M - 1) / 15, 0, 1)` | `0.60` |
  | `M-F003` | `KAL-F003` | データフロー密度リスク | `|E_dfg_unique| / (|V_var| * (|V_var| - 1))`。`E_dfg_unique` は変数ノード間の一意な `(source, target)` ペア集合。`|V_var| < 2` なら `0` | `clamp(raw_value, 0, 1)` | `0.45` |
  | `M-F004` | `KAL-F004` | 識別子反復リスク | `1 - H(tokens_multiset) / log2(|U|)`。`tokens_multiset` はローカル変数名と引数名を snake_case / camelCase 分割して重複を保持した多重集合、`U` はその一意トークン集合。`|U| < 2` なら `0` | `clamp(raw_value, 0, 1)` | `0.55` |

- **出力**: 各関数について `metric_id`, `raw_value`, `normalized_risk` を持つメトリクス集合
- **受け入れ基準**:
  - Given 有効な関数のCPG, When メトリクス算出を実行, Then `M-F001`〜`M-F004` のすべてが数値として算出される
  - Given 同一の関数CPG, When メトリクス算出を2回実行, Then `raw_value` と `normalized_risk` がビット単位で一致する
- **優先度**: Must
- **出典**: ユーザー明示 + 2026-03-19 設計判断
- **関連要件**: REQ-NF-003

#### REQ-FUNC-009: モジュールレベルメトリクスの算出

- **説明**: CPG のモジュール owner scope サブグラフに対して、v1 で固定したモジュールレベルメトリクスを算出する
- **入力**: 統一CPGのモジュール owner scope サブグラフ（Python/TypeScript は class、Rust は named module / file root module、Go は package）
- **処理**: v1 では以下の 3 メトリクスを必ず算出する

  | MetricId | RuleId | 名称 | `raw_value` | `normalized_risk` | デフォルト閾値 |
  |---|---|---|---|---|---|
  | `M-M001` | `KAL-M001` | モジュール fan-out リスク | 他モジュールへの一意な依存数 | `clamp(raw_value / 12, 0, 1)` | `0.50` |
  | `M-M002` | `KAL-M002` | 循環依存参加リスク | 当該モジュールが属する SCC のサイズ `s` に対し、非循環なら `0`、循環なら `(s - 1) / 5` | `clamp(raw_value, 0, 1)` | `0.20` |
  | `M-M003` | `KAL-M003` | 不安定性リスク | `fan_out / (fan_in + fan_out)`。分母が `0` なら `0` | `raw_value` | `0.75` |

- **出力**: 各モジュールについて `metric_id`, `raw_value`, `normalized_risk` を持つメトリクス集合
- **受け入れ基準**:
  - Given 有効なモジュールのCPG, When メトリクス算出を実行, Then `M-M001`〜`M-M003` のすべてが数値として算出される
- **優先度**: Must
- **出典**: ユーザー明示 + 2026-03-19 設計判断

#### REQ-FUNC-010: プロジェクトレベルメトリクスの算出

- **説明**: CPG 全体に対して、v1 で固定したプロジェクトレベルメトリクスを算出する
- **入力**: プロジェクト全体の統一CPG
- **処理**: v1 では以下の 3 メトリクスを必ず算出する

  | MetricId | RuleId | 名称 | `raw_value` | `normalized_risk` | デフォルト閾値 |
  |---|---|---|---|---|---|
  | `M-P001` | `KAL-P001` | 循環結合リスク | モジュール依存グラフのうち、サイズ 2 以上の SCC に属する依存辺数 `cyclic_edges / total_module_edges`。辺がなければ `0` | `raw_value` | `0.15` |
  | `M-P002` | `KAL-P002` | モジュールサイズエントロピー不均衡 | `1 - H(LOC_share) / log2(n)`。`n` は LOC > 0 のモジュール数。`n < 2` なら `0` | `raw_value` | `0.45` |
  | `M-P003` | `KAL-P003` | ハブ依存集中リスク | `max_in_degree / total_in_degree`。分母が `0` なら `0` | `raw_value` | `0.35` |

- **出力**: プロジェクト全体について `metric_id`, `raw_value`, `normalized_risk` を持つメトリクス集合
- **受け入れ基準**:
  - Given 有効なプロジェクトCPG, When メトリクス算出を実行, Then `M-P001`〜`M-P003` のすべてが数値として算出される
- **優先度**: Must
- **出典**: ユーザー明示 + 2026-03-19 設計判断

#### REQ-FUNC-011: 総合スコアの算出

- **説明**: 関数・モジュール・プロジェクトの各階層メトリクスを集約し、0〜100 の総合スコアを算出する
- **入力**: 全階層のメトリクス算出結果
- **処理**:
  1. 各スコープの `scope_risk` を、そのスコープに属する `participation = ScoredAndDiagnosable` な `normalized_risk` の算術平均として算出する。ただし、`enabled = false` のルールにバインドされたメトリクスは算術平均の母集団から除外する。あるスコープで対象メトリクスが全て除外された場合、そのスコープの `scope_risk` は `0.0`（リスクなし）とする
  2. 各階層の `level_risk` を、その階層に属する `scope_risk` の算術平均として算出する。プロジェクト階層は単一スコープなので、その `scope_risk` をそのまま用いる
  3. デフォルト重みは `function: 0.4`, `module: 0.35`, `project: 0.25` とし、設定ファイルで上書き可能とする。各重みは `> 0.0` かつ有限でなければならない。不正な値は設定エラー（exit code 2）とする。合計が `1.0` でない場合は比例再正規化する
  4. ある階層にスコープが 0 件の場合、その階層の重みは残る階層へ比例再配分する
  5. `scope_risk`, `level_risk`, `overall_risk` は各段階の算出直後に小数第 6 位で round-half-up し、その値をキャッシュと後続計算に用いる
  6. `overall_risk = Σ(adjusted_weight[level] * level_risk[level])`
  7. `function_score`, `module_score`, `project_score`, `overall_score` はそれぞれ `round_half_up(100 * (1 - risk))` で整数化する。ただしステップ 4 で重みが再配分された階層（スコープ 0 件）は `level_risk` が存在しないため、当該階層の `*_score` は `None`（機械可読出力では `null`）とする。`overall_score` は残存階層の再配分済み重みで算出するため常に存在する
  8. 機械可読出力では以下の 2 つの独立した理由により `*_score` が `null` に写像される: (a) `--level function|module|project` により報告対象外となった階層（Reporting 射影として省略。内部では全階層を算出する。REQ-FUNC-023 参照）、(b) 計算可能なスコープが存在しない階層（ステップ 4 / 7 参照）。`--level all` の場合でも (b) に該当する階層は `null` となる
  9. `overall_score` と各階層スコアは常に上記メトリクス集約の結果であり、`summary_scope`・診断件数・exit code 判定から逆算しない

- **出力**: 総合スコア（0〜100 の整数）および各階層の部分スコア（計算可能なスコープが存在する階層は 0〜100 の整数、存在しない階層は `None`）
- **受け入れ基準**:
  - Given 全階層のメトリクス結果, When 総合スコアを算出, Then 同一入力から常に同一の総合スコアと各階層スコアが出力される
  - Given 設定ファイルで重みを変更, When 総合スコアを算出, Then 変更後の重みと再配分規則で集約される
- **優先度**: Must
- **出典**: ユーザー確認済み + 2026-03-19 設計判断
- **関連要件**: REQ-FUNC-008, REQ-FUNC-009, REQ-FUNC-010

#### REQ-FUNC-012: メトリクス定義のプラグイン拡張

- **説明**: ユーザーが独自のメトリクス定義を追加できる拡張機構を提供する
- **入力**: `.kalos.toml` で登録された WASM プラグインモジュール参照（ワークスペースルート相対 `path`, `sha256`）と、SPI version `kalos-metric-spi-v1` の ABI 契約に準拠したメトリクス定義（normative ABI 仕様は ADR-0004 [§host exports](adr/0004-wasm-metric-plugin-runtime.md#host-exports) / [§read ヘルパー戻り値契約](adr/0004-wasm-metric-plugin-runtime.md#read-ヘルパー戻り値契約) / [§ptr/len エンコーディング契約](adr/0004-wasm-metric-plugin-runtime.md#ptrlen-エンコーディング契約) / [§ScopeId 直列化契約](adr/0004-wasm-metric-plugin-runtime.md#scopeid-直列化契約) / [§線形メモリデータレイアウト](adr/0004-wasm-metric-plugin-runtime.md#線形メモリデータレイアウト) / [§スカラー戻り値エンコーディング](adr/0004-wasm-metric-plugin-runtime.md#スカラー戻り値エンコーディング) / [§SPI v1 列挙契約](adr/0004-wasm-metric-plugin-runtime.md#spi-v1-列挙契約) を参照）
- **処理**: Configuration は `.kalos.toml` のプラグイン登録を `workspace_relative_path` と checksum から決定論的な `plugin_manifest` へ正規化する。この段階で `WorkspaceRoot` 外 path や不正な `sha256` は設定エラー（exit code 2）とする。Plugin Host は `plugin_manifest` を `workspace_relative_path` 昇順でロードし、stable `metric_id`, `level`, `name`, `description` を持つ `MetricDefinition` を登録する。`metric_id` は組み込みメトリクスと先行ロード済みプラグインを含めてグローバル一意でなければならず、衝突したモジュールは deterministic なロード失敗として warning を出してスキップする。Plugin Host は登録済み `MetricDefinition` を `level` に一致する各 `ScopeId` ごとに評価し、入力には `UnifiedCpg.subgraph(scope_id)` の read-only view を渡す。function/module metric は該当 scope ごとに 1 回ずつ、project metric は正規形 `ScopeId(level = Project, qualified_name = "<project>", file_path = ".")` に対して 1 回だけ評価する。v1 では `participation = ReportOnly` として扱う。評価時は登録済みプラグインを Metrics パイプラインへ統合し、invocation ごとに `per-invocation fuel budget = 500_000 fuel`（参考: ~50ms）、`linear_memory_limit = 64 MiB`、実行全体では Metrics stage budget の内数として `aggregate fuel budget = 30_000_000 fuel`（全解析、参考: ~3s）/ `5_000_000 fuel`（diff mode、参考: ~0.5s）を適用し、ネットワーク・ファイル書込を禁止する。diff mode から全解析へフォールバックした場合は全解析の budget（`30_000_000 fuel`）を適用する（fuel が規範的上限であり、壁時間は参考値。上記の具体的数値は暫定値であり、PoC ベンチマークで検証のうえ v1 リリースまでに確定する。ADR-0004 参照）。プラグインファイル読込失敗、checksum 不一致、SPI version 不一致、fuel budget 超過、メモリ超過は当該プラグイン評価のみを打ち切り、aggregate fuel budget 超過時は残りのプラグイン評価を warning 付きでスキップする。いずれも `stderr` と構造化ログへ運用警告を出す。失敗またはスキップしたプラグインはその実行で `MetricValue` を返さず、v1 ではプラグインメトリクスは `metrics` 出力のみに現れ、診断・総合スコア・exit code には影響させない。diff mode では、現在の実行で正常にロード・評価されなかったプラグインの baseline cache 済み `MetricValue` も `metrics` 出力から除外し、stale なプラグインメトリクスを部分的に再利用しない
- **受け入れ基準**:
  - Given プラグイン仕様に準拠したメトリクス定義, When 解析実行, Then 当該メトリクスが `metrics` 出力へ追加され、組み込みの診断・総合スコア・exit code 契約は変化しない
  - Given プラグインが既定上限を超過, When 解析実行, Then 当該プラグイン評価は失敗として打ち切られ、kalos 本体の実行は継続する
  - Given プラグインのロードまたは検証に失敗, When 解析実行, Then 当該失敗は運用警告として記録されるだけで、既存の診断・総合スコア・exit code の契約は変わらない
  - Given aggregate plugin budget を使い切った, When 解析実行, Then 残りのプラグイン評価は warning 付きでスキップされ、コア評価は継続する
  - Given プラグインの `metric_id` が組み込みまたは先行ロード済みプラグインと衝突, When 解析実行, Then 当該モジュールは初期化中に登録した全 `MetricDefinition` をロールバックされ warning 付きでスキップされる（登録の原子性）。同じ `plugin_manifest` から常に同じ結果になる
- **優先度**: Should
- **出典**: ユーザー確認済み（当初Couldだったが、ユーザーの要望でShouldに昇格）

### 3.3 診断・改善提案

#### REQ-FUNC-013: メトリクス閾値違反の診断報告

- **説明**: `participation = ScoredAndDiagnosable` な各メトリクスの `normalized_risk` をルールごとの閾値と比較し、違反をメトリクス診断として報告する
- **入力**: メトリクス算出結果、ルールごとの閾値設定
- **処理**: `participation = ScoredAndDiagnosable` な各メトリクスの `normalized_risk` を閾値と比較し、超過があれば `kind = "metric"` の診断オブジェクトを生成する。v1 の plugin metric（`participation = ReportOnly`）はメトリクス診断の対象外とする
- **出力**: メトリクス診断オブジェクトのリスト。各診断は共通フィールド `rule_id`, `severity`, `location`, `message`, `template_suggestion` と、`metric` フィールド `{ metric_id, raw_value, normalized_risk, threshold, overflow_ratio }` を持つ。内部的には各診断が canonical `primary_scope_id` を持ち、metric 診断では評価対象 `ScopeId` と一致する。diff mode の診断一覧判定と baseline の `ScopeDiagnosticSnapshot` への帰属はこの `primary_scope_id` で決定する。単一ファイルへ結び付かない cross-scope 診断では、`location` は根拠 scope 群のうち辞書順最小 `file_path` の `start_line = 1`, `end_line = 1`, `column = null` を代表位置として用いる。human 形式ではこの位置を `path:line`（`line` には `location.start_line` の値を使う）と表示し、SARIF では `startColumn` / `endColumn` を出力しない
- **受け入れ基準**:
  - Given 関数のCFGエントロピーが閾値を超過, When 診断実行, Then 当該関数の位置・ルールID・重大度・メトリクス値・閾値を含む診断が報告される
  - Given すべてのメトリクスが閾値内, When 診断実行, Then 診断は0件で正常終了する
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-014: 構造的パターンの検出

- **説明**: CPG から v1 で固定したソフトウェア設計上の問題パターンを検出する
- **入力**: 統一CPG と既算出メトリクス
- **処理**: 各パターンルールは `evaluation_scope`（Function / Module / Project）を持ち、呼び出し粒度を決定する。scope-level ルール（Function / Module）はスコープ候補ごとにサブグラフを受け取って評価し、project-level ルールは CPG 全体のビューを受け取って 1 回だけ評価する。v1 では以下のパターンルールを適用する

  | RuleId | パターン | evaluation_scope | 対象 | 検出条件 | デフォルト重大度 |
  |---|---|---|---|---|---|
  | `KAL-PAT001` | God Unit | Module | `AnalysisLevel.Module` の owner scope（Python/TypeScript は class、Rust は named module または file root module、Go は package） | 対象 owner scope が `public_member_count >= 20` かつ `fan_out >= 8` かつ配下関数の `M-F002` 平均 `>= 0.50` | `warning` |
  | `KAL-PAT002` | Feature Envy | Function | 関数 | 外部オブジェクト/モジュールへの参照数が 5 以上かつ `foreign_accesses / (foreign_accesses + local_accesses) >= 0.70` | `warning` |
  | `KAL-PAT003` | Circular Dependency | Project | モジュール依存グラフ全体 | SCC のサイズが 2 以上（1 回の評価で複数の SCC を検出し、SCC ごとに診断を生成する） | `error` |

- **言語別の計数規則**:
  - `KAL-PAT001` は `--level all` または `--level module` のときのみ評価し、`PatternEvidence.evidence_scopes` には対象 owner scope を `ScopeId(level = Module)` として格納する
  - `public_member_count` は TypeScript では対象 class の public メソッド・public フィールド数（constructor, private, protected を除く）とし、Python では class body へ直接宣言されたメンバーのうち論理名が `_` で始まらない public method・class attribute・property descriptor 数を数える。Python の public method には `@property` / `@cached_property` / setter / deleter を含めず、property 系は public field 相当として 1 件だけ数える。Python の `__init__` と dunder method は除外し、メソッド本体内で初めて代入される instance attribute は数えない。Rust では対象 module/file root 直下の `pub` な top-level item 数、Go では対象 package 直下の exported top-level declaration 数とする
  - `foreign_accesses` は「現在の関数が所属する owner scope（class / module / package）以外」への参照・呼び出し数、`local_accesses` は同一 owner scope 内への参照・呼び出し数とする。Python/TypeScript の `self` / `this`、Rust の `self` / `Self` / 同一 module 内 item、Go の同一 package 内識別子参照は local に数える
- **出力**: `kind = "pattern"` の診断オブジェクトのリスト。各診断は共通フィールド `rule_id`, `severity`, `location`, `message`, `template_suggestion` に加え、`pattern` フィールド `{ pattern_type, evidence_scopes, evidence_message }` を持つ。内部的には各診断が canonical `primary_scope_id` を持ち、rule の主対象 scope を優先する。単一の主対象 scope が定義できない cross-scope 診断では `evidence_scopes` の辞書順最小 `ScopeId` を `primary_scope_id` とする。diff mode の診断一覧判定と baseline の `ScopeDiagnosticSnapshot` への帰属はこの `primary_scope_id` で決定する。単一ファイルへ結び付かない cross-scope 診断では、`location` は `evidence_scopes` のうち辞書順最小 `file_path` の `start_line = 1`, `end_line = 1`, `column = null` を代表位置として用いる。PAT001 の `M-F002` 平均は対象 owner scope 配下関数の既算出結果から求める。human 形式では `column = null` の位置を `path:line`（`line` には `location.start_line` の値を使う）と表示し、SARIF では `startColumn` / `endColumn` を出力しない
- **受け入れ基準**:
  - Given 過度に多くの責務を持つ module owner scope, When `--level module` または `--level all` で診断実行, Then `KAL-PAT001` として検出される
  - Given 関数の `foreign_accesses >= 5` かつ `foreign_accesses / (foreign_accesses + local_accesses) >= 0.70`, When 診断実行, Then `KAL-PAT002` として検出される
  - Given 関数の `foreign_accesses / (foreign_accesses + local_accesses) < 0.70`, When 診断実行, Then その関数に対して `KAL-PAT002` は報告されない
  - Given モジュール依存グラフに循環がある, When 診断実行, Then `KAL-PAT003` として検出される
- **優先度**: Should
- **出典**: ユーザー確認済み + 2026-03-19 設計判断

#### REQ-FUNC-015: 具体的な改善提案テキストの生成

- **説明**: 各診断に対して、何が問題か・なぜ問題か・どう改善すべきかを含む具体的な改善提案テキストを生成する。テンプレートベースの生成を基本とし、オプションでLLM連携による文脈に即した提案生成を提供する
- **入力**: Application Pipeline が `Diagnostic` と `SourceAnalysis` から組み立てた allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, workspace_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }`。`rule_id`, `severity`, `workspace_relative_path` は `Diagnostic` から取得し、`language` は代表ファイル（`Diagnostic.location.file_path`）に対応する `SourceAnalysis.source_files` メタデータから解決する。`source_excerpt` / `cpg_excerpt` は代表ファイルに還元できる対象スコープの CPG・ソースから取得し、request を生成する場合はどちらか一方だけを含める。`metric` と `pattern` は `Diagnostic.kind` に応じて排他的に設定される
- **処理**:
  - テンプレートモード（デフォルト）: 違反パターンごとの定型テンプレートにコード文脈を埋め込んで提案文を生成する
  - LLM連携モード（`--llm` オプション）: Application Pipeline は `Diagnostic` と `SourceAnalysis` から allowlist 済み `LlmEnrichmentRequest` を組み立てて LLM に渡す。許可するのは `rule_id`, `severity`, `language`, `workspace_relative_path`, `metric` または `pattern`, `source_excerpt` または正規化済み `cpg_excerpt` のみとし、`source_excerpt` と `cpg_excerpt` は request ごとに相互排他的とする。それ以外の診断内部情報は送信しない。テンプレートベースの結果も併記する。LLM非応答、タイムアウト、代表ファイルの言語を一意に解決できない場合、または multi-file / multi-language 診断の必須根拠を代表ファイル断片へ還元できない場合は `llm_suggestion` を付与せず、テンプレート結果だけを返す
- **出力**: 各診断に対し `template_suggestion`（必須）を生成し、`--llm` 指定時は出力境界で `llm_suggestion`（任意）を併記する
- **受け入れ基準**:
  - Given CFGエントロピー超過の診断, When テンプレートモードで改善提案を生成, Then 「この関数は分岐が複雑すぎる。条件分岐を抽出関数に分離することで複雑度を低減できます」のような具体的な提案が出力される
  - Given 循環依存の診断, When 改善提案を生成, Then 循環に関与するモジュールの依存方向を示し、依存逆転の具体的な方針を提案する
  - Given `--llm` 指定, When LLM提案を生成, Then 送信対象は当該診断に必要な最小ソース断片または `CpgSubgraphExcerpt` に限定され、プロジェクト全体は送信されない
  - Given `--llm` 指定, When 改善提案を出力, Then `template_suggestion` と `llm_suggestion` が区別可能な形で併記される
  - Given LLM連携モードで非応答, When 改善提案を生成, Then テンプレートモードの結果にフォールバックする
- **優先度**: Must
- **出典**: ユーザー明示 + 2026-03-19 設計判断
- **関連要件**: REQ-NF-008

#### REQ-FUNC-016: 診断への重大度付与

- **説明**: 各診断に閾値超過度合いまたはパターン種別に応じた重大度を付与する
- **入力**: 診断オブジェクト、重大度判定基準（設定ファイルでカスタマイズ可能）
- **処理**:
  - メトリクス診断では `overflow_ratio = (normalized_risk - threshold) / max(1 - threshold, 1e-9)` を用いる
  - メトリクス診断のデフォルト重大度は `overflow_ratio < 0.25` なら `info`、`0.25 <= overflow_ratio < 0.60` なら `warning`、`0.60 <= overflow_ratio` なら `error` とする
  - パターン診断のデフォルト重大度は `KAL-PAT001 = warning`, `KAL-PAT002 = warning`, `KAL-PAT003 = error` とする
  - `rules.<RuleId>.severity` が設定されている場合、そのルールの最終 `Diagnostic.severity` は診断種別に関係なく当該値で上書きする。metric 診断では `overflow_ratio` から導出したデフォルト重大度の後に override を適用し、pattern 診断ではデフォルト重大度の後に override を適用する
- **重大度定義**:
  - error: プロジェクトの品質基準を明確に逸脱（CI/CDでfailの根拠になる）
  - warning: 改善が強く推奨される
  - info: 改善の余地があるが許容範囲内
- **受け入れ基準**:
  - Given `overflow_ratio >= 0.60`, When 重大度判定, Then `error` が付与される
  - Given `0.25 <= overflow_ratio < 0.60`, When 重大度判定, Then `warning` が付与される
  - Given `0 < overflow_ratio < 0.25`, When 重大度判定, Then `info` が付与される
  - Given `KAL-PAT003`, When 重大度判定, Then デフォルトで `error` が付与される
  - Given `rules.KAL-F001.severity = "warning"` かつ `overflow_ratio >= 0.60`, When `KAL-F001` の重大度判定, Then 最終 `Diagnostic.severity` は `warning` となる
- **優先度**: Must
- **出典**: ユーザー確認済み + 2026-03-19 設計判断

#### REQ-FUNC-017: 改善提案へのコード例の付与

- **説明**: 改善提案にリファクタリング後のコードスケッチ（擬似コードまたは実コード断片）を含める
- **入力**: 診断オブジェクト + 当該診断に対応する `CpgSubgraphExcerpt` または最小ソース断片
- **処理**: 違反パターンに応じたリファクタリングパターンを適用し、改善後のコード概要を生成する
- **受け入れ基準**:
  - Given 関数分割が推奨される診断, When コード例を生成, Then 分割後の関数シグネチャと呼び出し構造の概要が提示される
- **優先度**: Should
- **出典**: ユーザー確認済み

### 3.4 CLIインターフェース

#### REQ-FUNC-018: `kalos check` コマンドによる解析実行

- **説明**: `kalos check [<path>...]` で対象パスを解析する。CPG抽出→メトリクス算出→診断生成→結果出力の全パイプラインを統合する
- **入力**: 0 個以上の解析対象パス（ファイルまたはディレクトリ）とオプション引数。位置引数を省略した場合は `WorkspaceRoot`（正規形 `["."]`）を暗黙の対象とし、全ワークスペース解析として扱う（ADR-0003 参照）
- **一覧・summary・exit code の母集団**:
  - 診断一覧: non-diff モードでは選択された `--level` に対する完全な診断集合、diff mode では `AffectedScopeSet` に属する診断のみ
  - `--severity` は一覧の表示/出力対象だけを絞り込み、summary と exit code の計算母集団は変えない
  - `--level all`（デフォルト）では、summary と exit code は解決済み `analysis_targets` 内の全階層の診断集合を母集団とする（`SummaryScope.WholeProject`）
  - `--level <function|module|project>` 指定時は、指定階層の診断のみを母集団とする（REQ-FUNC-023 参照）
  - summary は `summary_scope` に応じて Application Pipeline が materialize する。`summary_scope = listed_diagnostics` では現在の診断一覧から、diff mode かつ `summary_scope = whole_project` では merged post-change `ScopeDiagnosticSnapshot` から再構成する
- **主要オプション**:
  - `--format <human|json|sarif>`: 出力形式（デフォルト: human）
  - `--level <function|module|project|all>`: 解析階層（デフォルト: all）
  - `--config <path>`: 明示的に使用する `.kalos.toml` のパス。この親ディレクトリを `WorkspaceRoot` とする
  - `--exclude <pattern>`: 除外パターン
  - `--severity <error|warning|info>`: 表示する最低重大度
  - `--diff <base-ref>`: 変更ファイル再抽出 + ベースライン再利用による差分解析
  - `--llm`: LLM連携による改善提案を有効化（non-diff / diff 両モードで動作する。diff mode では `AffectedScopeSet` に属する診断のみをエンリッチ対象とする）
  - `--strict`: warning を error 相当の exit code 判定対象にする（診断オブジェクトの `severity` 自体は変更しない）
- **受け入れ基準**:
  - Given 有効なプロジェクトディレクトリ, When 位置引数なしで `kalos check` を実行, Then WorkspaceRoot（正規形 `["."]`）をデフォルト対象として全ワークスペースが解析され、診断結果が端末に表示される。この実行は全ワークスペース解析としてベースライン生成（write-back）の対象となる（ベースラインの消費は `--diff` 実行時のみ）
  - Given `kalos check .` のように位置引数 `.` を明示的に指定, When 解析実行, Then CLI は「位置引数が明示指定された」と判定し（`ProjectConfig.targets_explicitly_specified = true`）、`analysis_targets` は部分集合として扱われる（ADR-0003: 明示指定は `WorkspaceRoot` 配下の網羅性を判定せず常に部分集合）。この実行はベースラインを生成も消費もしない
  - Given `kalos check src tests/test_app.py` のように複数 path を指定, When 解析実行, Then 指定された各 path 配下の対応言語ファイルが単一の解析対象集合として統合され、部分集合として扱われる
  - Given `--format json` 指定, When 解析実行, Then 結果がJSON形式で出力される
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-019: 人間可読な結果表示

- **説明**: 解析結果を共通フィールドと診断種別ごとの詳細フィールドを含む形式で端末に表示する
- **出力形式例**:
  ```
  src/parser.rs:42:1  warning[KAL-F001]  [metric] 関数 `parse_expression` の CFG 分岐エントロピーリスクが閾値を超過
    metric=M-F001 raw=0.667 normalized=0.667 threshold=0.550 overflow=0.260
    template → 条件分岐を match アームごとに抽出関数へ分離することで複雑度を低減できます
    llm      → 分岐条件のグループごとに補助関数へ切り出すとテスト単位も小さくできます

  src/lib.rs:1  error[KAL-PAT003]  [pattern] モジュール間に循環依存が存在する
    evidence=parser -> lexer -> parser
    template → 共有データ型を独立モジュール `ast_types` に抽出し、依存方向を一方向に固定してください

  ── Summary ──────────────────────────
  Score: 72/100  (function: 68, module: 75, project: 78)
  3 errors, 5 warnings, 12 info
  ```
- **受け入れ基準**:
  - Given メトリクス診断, When human形式で出力, Then 共通フィールドに加えて `metric_id`, `raw_value`, `normalized_risk`, `threshold`, `overflow_ratio`, 改善提案が含まれる
  - Given パターン診断, When human形式で出力, Then 共通フィールドに加えて `pattern_type`, `evidence_message`, 改善提案が含まれる
  - Given `--llm` 指定, When human形式で出力, Then `template` と `llm` の提案が別ラベルで表示される
  - Given 端末がカラー対応, When human形式で出力, Then 重大度に応じた色分けが適用される（error: 赤, warning: 黄, info: 青）
  - Given cross-scope 診断で `location.column = null`, When human形式で出力, Then synthetic な列番号は補完せず `path:line` 形式で表示される
  - Given `--level all`（デフォルト）で解析完了, When human形式で出力, Then 末尾に解決済み `analysis_targets` 全体の総合スコアサマリーと重大度別件数が表示される
  - Given `--level function` で解析完了, When human形式で出力, Then 末尾に関数レベルメトリクスから算出した総合スコアと、関数レベル診断のみを母集団とした重大度別件数が表示され、module/project のスコアは表示されない
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-020: JSON形式での結果出力

- **説明**: 解析結果を機械可読なJSON構造で出力する。`metrics` は `--level` で選択された対象階層のメトリクスのみを含み、非対象階層のメトリクスは含めない（must exclude）。`diagnostics` は non-diff モードでは選択された `--level` に対する完全な診断集合、diff mode では `AffectedScopeSet` に属する診断部分集合を返す。総合スコア・summary・`schema_version` を持つ
- **最低限のJSON契約**:
  - ルートには `schema_version`, `analysis_targets`, `scores`, `metrics`, `diagnostics`, `diagnostics_scope`, `summary`, `summary_scope`, `tool_version` を必須とする
  - `schema_version` の初期値は `"1.0.0"` とする。バンプポリシー: payload shape とセマンティクスの双方に影響しない明確化・注記追加は patch、後方互換な optional フィールド追加は minor、フィールド削除・型変更・必須化・既存フィールドのセマンティクス変更は major とする
  - `analysis_targets` は CLI で受け取った解析対象 path 群を Configuration が `WorkspaceRoot` 基準で正規化・検証した配列とし、入力順を保持する。位置引数省略時のデフォルト `.` も同様に正規化する。単一 target の場合も配列で表現する
  - `metrics` には組み込みメトリクスとプラグインメトリクスの両方を含める。`--level` に従い対象階層のメトリクスのみを射影し、非対象階層のメトリクスは含めない（must exclude）。この射影は Reporting コンテキストが `ReportViewOptions.requested_level` に基づいて担う。各メトリクスエントリには `participation` フィールド（`"scored_and_diagnosable"` | `"report_only"`）を付与し、当該メトリクスがスコアリング・診断に参加するか report 専用かを識別可能にする。v1 のプラグインメトリクスは `participation = "report_only"` であり `diagnostics[*]`、`scores`、exit code の判定母集団には含めない
  - `diagnostics[*]` は `kind` を discriminant とし、`kind = "metric"` なら `metric` オブジェクト、`kind = "pattern"` なら `pattern` オブジェクトを必須とする
  - `diagnostics[*].template_suggestion` は必須、`diagnostics[*].llm_suggestion` は任意とする
  - `scores` には `overall`, `function`, `module`, `project` を必須とする。`overall` は `--level all` の場合は常に 0〜100 の整数（REQ-FUNC-011 の全階層メトリクス集約）、`--level function|module|project` の場合は指定階層スコアの射影であり計算可能なスコープが存在しない場合は `null` となる。`summary` や診断件数から逆算しない。`function` / `module` / `project` は、対象階層かつ計算可能なスコープが存在する場合は 0〜100 の整数、非対象階層または計算可能なスコープが存在しない場合は `null` とする
  - `diagnostics_scope` は `whole_project | affected_only` とする。`whole_project` は non-diff モードで「選択された `--level` に関して、解決済み `analysis_targets` 内の診断集合が完全である」ことを表す（未選択階層の診断欠落を意味しない）。`affected_only` は diff mode で `AffectedScopeSet` に属する診断のみを含むことを表す。`summary_scope` は `whole_project | listed_diagnostics` とする。`whole_project` は解決済み `analysis_targets` 内の全階層の診断を母集団とする
- **受け入れ基準**:
  - Given 解析結果, When `--format json` で出力, Then 出力が有効なJSONであり、上記の必須フィールドがすべて存在する
  - Given `--level all` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "whole_project"` かつ `summary_scope = "whole_project"` となり、`scores.function/module/project` は計算可能なスコープが存在する階層では整数、存在しない階層では `null` となる
  - Given `--level function` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "whole_project"` かつ `summary_scope = "listed_diagnostics"` となり、`scores.overall` と `scores.function` は計算可能な function スコープが存在する場合は整数（存在しない場合はいずれも `null`）、`scores.module` と `scores.project` は `null` となり、`metrics` には関数レベルのメトリクスのみが含まれ module / project レベルのメトリクスは含まれない
  - Given `--diff <base-ref> --level all` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "affected_only"` かつ `summary_scope = "whole_project"` となる
  - Given `--diff <base-ref> --level function` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "affected_only"` かつ `summary_scope = "listed_diagnostics"` となり、`scores.overall` と `scores.function` は計算可能な function スコープが存在する場合は整数（存在しない場合はいずれも `null`）、`scores.module` と `scores.project` は `null` となり、`metrics` には関数レベルのメトリクスのみが含まれ module / project レベルのメトリクスは含まれない
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-021: SARIF形式での結果出力

- **説明**: 解析結果をSARIF（Static Analysis Results Interchange Format）形式で出力し、GitHub Code Scanning等との連携を可能にする
- **SARIF への写像方針**:
  - **ルール**: 各 `Diagnostic.rule_id` を `run.tool.driver.rules[]` に登録する。`rules[].id` は `rule_id`（例: `KAL-F001`）、`rules[].shortDescription.text` はルール名称、`rules[].defaultConfiguration.level` はデフォルト重大度の SARIF level 写像とする。`result.ruleId` で当該ルールを参照し、`result.ruleIndex` でインデックスを指定する
  - **重大度**: `Diagnostic.severity` を `result.level` へ写像する。`error` → `"error"`、`warning` → `"warning"`、`info` → `"note"` とする
  - **位置**: `Diagnostic.location` を `result.locations[].physicalLocation` へ写像する。`artifactLocation.uri` は `WorkspaceRoot` 相対パス、`region.startLine` は `location.start_line`、`region.endLine` は `location.end_line` とする。`location.column` が非 `null` の場合は `region.startColumn` を出力し、`location.column = null` の場合は `startColumn` / `endColumn` を出力しない
  - **メッセージ**: `Diagnostic.message` は常に `result.message.text` へ格納する
  - **改善提案**: `template_suggestion` は常に `result.properties.kalos.template_suggestion` へ格納する。`llm_suggestion` が存在する場合は `result.properties.kalos.llm_suggestion` に格納する
- **受け入れ基準**:
  - Given 解析結果, When `--format sarif` で出力, Then 出力がSARIF 2.1.0スキーマに準拠する
  - Given メトリクス閾値違反の診断, When `--format sarif` で出力, Then `run.tool.driver.rules[]` に当該 `rule_id` が登録され、`result.ruleId` が一致し、`result.level` が重大度の SARIF 写像と一致する
  - Given `location.column` が非 `null` の診断, When `--format sarif` で出力, Then `result.locations[].physicalLocation.region` に `startLine`, `endLine`, `startColumn` が含まれる
  - Given `location.column = null` の cross-scope 診断, When `--format sarif` で出力, Then `result.locations[].physicalLocation.region` に `startLine` と `endLine` は含まれるが `startColumn` / `endColumn` は含まれない
  - Given GitHub Code Scanning に SARIF をアップロード, When PR 上で結果を表示, Then 各診断がルール ID・重大度・ソース位置付きのアノテーションとして表示される
- **優先度**: Should
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-020

#### REQ-FUNC-022: Exit codeによるパイプライン制御

- **説明**: 解析結果に応じたexit codeを返し、CI/CDパイプラインでのpass/fail判定を可能にする
- **判定母集団**: `--level all`（デフォルト）では exit code は解決済み `analysis_targets` 内の全階層の診断集合を基準とし、`--severity` による表示フィルタの影響を受けない。`--level` で階層を限定した場合は指定階層の診断を基準とする（REQ-FUNC-023）。diff mode でも同様とする
- **厳格モード**: `--strict` は exit code 判定だけを変更する追加ポリシーであり、`Diagnostic.severity`、summary 件数、JSON/SARIF に出力される重大度、`--severity` による表示フィルタの意味は変更しない

  | 状況 | Exit code |
  |---|---|
  | 診断0件、またはwarning以下のみ | 0 |
  | error 1件以上 | 1 |
  | kalos自体の実行エラー | 2 |

- **受け入れ基準**:
  - Given error診断が1件以上, When 解析完了, Then exit code 1で終了する
  - Given warning以下のみ, When 解析完了, Then exit code 0で終了する
  - Given `--strict` オプション指定かつwarning 1件以上, When 解析完了, Then exit code 1で終了する
  - Given CPG抽出エンジンの実行失敗, When 解析実行, Then exit code 2で終了する
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-023: 解析階層の選択

- **説明**: `--level` オプションで報告対象の階層を限定する。CLI Shell がオプションを解釈し、Application Pipeline は指定階層を出力・summary・exit code の対象にする。内部では常に全階層（function / module / project）のメトリクス算出・診断生成を実行する（ベースラインキャッシュの保存不変条件として全階層の結果が必要なため。ADR-0003 参照）。`--level` による非対象階層の報告除外は Reporting コンテキストが `ReportViewOptions.requested_level` に基づいて担う。CPG 抽出は全ファイルを対象とする（階層横断の依存解決に必要なため）
- **パイプライン動作**:
  - `--level all`（デフォルト）: 全階層のメトリクス・診断を算出し、総合スコアを報告する。`summary_scope = "whole_project"`（解決済み `analysis_targets` 内の全階層を母集団とする）
  - `--level function|module|project`: 指定階層のメトリクス・診断を報告する。全階層は常に内部的に算出されるが、非対象階層は報告に含めない（must exclude）。総合スコアは指定階層の `level_risk` から算出する（指定階層に計算可能なスコープが存在しない場合は `null`）。`summary_scope = "listed_diagnostics"` は summary と exit code の母集団だけを規定し、`scores.overall` 自体は診断件数から再計算しない。機械可読出力では `scores.overall` をその総合スコアとし、非対象階層の `scores.*` は `null` とする
  - `AnalysisLevel.Module` は言語ごとの owner scope を表し、Python/TypeScript の class、Rust の module / file root module、Go の package を含む。`KAL-PAT001` のような owner-scope パターンは `--level module|all` のときのみ評価対象とする
- **受け入れ基準**:
  - Given `--level function` 指定, When 解析実行, Then 関数レベルのメトリクスと診断のみが出力される
  - Given `--level function` かつ `--format json`, When 解析実行, Then `summary_scope = "listed_diagnostics"` となり、`scores.module` と `scores.project` は `null` となる
- **優先度**: Should
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-018, REQ-FUNC-020

#### REQ-FUNC-024: 総合スコアサマリーの表示

- **説明**: 解析結果の末尾に総合スコア・各階層スコア・重大度別件数のサマリーを表示する
- **サマリー母集団**: `--level all`（デフォルト）では summary は解決済み `analysis_targets` 内の全階層の診断集合を基準とし、`--severity` による表示フィルタの影響を受けない。`--level` で階層を限定した場合は指定階層の診断を基準とする（REQ-FUNC-023）。表示される総合スコア自体は REQ-FUNC-011 のメトリクス集約結果を用いる
- **materialization 契約**: summary は `DiagnosticReport` の内部で再計算しない。Application Pipeline が `summary_scope` に応じて materialize し、`summary_scope = whole_project` の diff mode では merged post-change `ScopeDiagnosticSnapshot` から重大度別件数を再構成する
- **受け入れ基準**:
  - Given `--level all` で解析完了, When 結果を出力, Then 総合スコア（0〜100）・各階層スコア・重大度別診断件数が表示される
  - Given `--level function` で解析完了, When 結果を出力, Then 総合スコア・関数階層スコア・重大度別診断件数が表示され、module/project のスコアは表示されない
- **優先度**: Must
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-011

### 3.5 設定・ルール管理

#### REQ-FUNC-025: プロジェクト設定ファイルの読み込み

- **説明**: `.kalos.toml` からルール・閾値・除外パターン・スコア重み・プラグイン登録を読み込む。Configuration は `--config <path>` 指定時はその `.kalos.toml` を明示的に読み込み、その親ディレクトリを `WorkspaceRoot` とする。`--config` 未指定時はカレントディレクトリから親方向に設定ファイルを探索し、最初に見つかった `.kalos.toml` の親ディレクトリを `WorkspaceRoot` とする。`.kalos.toml` が見つからない場合は最初に見つかった `.git` の親ディレクトリ、どちらも見つからない場合は実行時カレントディレクトリを `WorkspaceRoot` とする。Configuration は内部 `FilePath`、`workspace_relative_path`、`plugin_manifest`、`analysis_targets` をこの `WorkspaceRoot` 基準で正規化する。`analysis_targets` については CLI Shell から受け取った生パス（位置引数省略時のデフォルト `.` を含む）を `WorkspaceRoot` 相対パスへ正規化し、canonicalize 後に `WorkspaceRoot` 配下に入らないパスは入力エラーとして拒否する（monorepo対応）。プラグイン `path` も同様に `WorkspaceRoot` 配下に限定する
- **設定の優先順位**: スカラー値は CLI引数 > プロジェクト設定ファイル > デフォルト値。`[general].exclude`（配列）は `.gitignore` 既定値 + 設定ファイル + CLI の加算マージとし、プラグイン登録は `workspace_relative_path` と checksum を含む `plugin_manifest` へ正規化して保持する
- **設定ファイル形式例**:
  ```toml
  [general]
  exclude = ["vendor/**", "generated/**"]

  [rules.KAL-F001]
  enabled = true
  severity = "warning"
  threshold = 0.60

  [rules.KAL-PAT003]
  enabled = false

  [score.weights]
  function = 0.4
  module = 0.35
  project = 0.25

  [[plugins]]
  path = ".kalos/plugins/halstead.wasm"
  sha256 = "4f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d"
  ```
- **設定値の検証**: `score.weights.*` は `> 0.0` かつ有限、`rules.<RuleId>.threshold` は `[0.0, 1.0]` の閉区間、`rules.<RuleId>.severity` は `error | warning | info` のいずれか、`plugins[*].sha256` は 64 文字の16進文字列でなければならない。検証失敗時は設定エラーとして exit code 2 で終了する
- **受け入れ基準**:
  - Given `.kalos.toml` が存在, When 解析実行, Then 設定ファイルの内容がルール・閾値に反映される
  - Given `--config services/api/.kalos.toml` を指定, When 解析実行, Then そのファイルが読み込まれ、`services/api` が `WorkspaceRoot` として採用される
  - Given 親ディレクトリに `.kalos.toml` が存在, When 解析実行, Then その親ディレクトリが `WorkspaceRoot` として採用され、内部パスはすべてそこからの相対パスに正規化される
  - Given CLI引数と設定ファイルが競合, When 解析実行, Then CLI引数が優先される
  - Given `.kalos.toml` にプラグイン登録がある, When 解析実行, Then path と checksum から決定論的な `plugin_manifest` が解決される
  - Given `.kalos.toml` がなく親方向に `.git` がある, When 解析実行, Then `.git` の親ディレクトリが `WorkspaceRoot` として採用される
  - Given canonicalize 後に `WorkspaceRoot` 配下へ入らない target path または plugin path, When 解析実行, Then エラーメッセージを表示し exit code 2で終了する
  - Given 設定ファイルに構文エラー, When 解析実行, Then エラーメッセージと該当箇所を表示し exit code 2で終了する
  - Given `score.weights.function = -0.1` または `rules.KAL-F001.threshold = 1.5`, When 解析実行, Then 設定エラーとして exit code 2で終了する
  - Given 設定ファイルなし, When 解析実行, Then デフォルト値で動作する
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-026: 個別ルールの有効/無効切り替え

- **説明**: 設定ファイルで個別ルールの `enabled` を `false` に設定することで、当該ルールの全効果を抑制する。影響範囲はルール種別により異なる
  - **メトリクスルール**（例: `KAL-F001`）: メトリクス計算は実行する（他ルールの内部依存のため）。`metrics` 出力にも含む（計算結果の観測は維持）。ただし、診断は生成せず、`scope_risk` 集約への参加を除外する（REQ-FUNC-011 ステップ 1 参照）。結果として `scores`・`summary` 件数・`exit code` に影響しない
  - **パターンルール**（例: `KAL-PAT003`）: パターン検出自体を実行しない。診断は生成されず、`summary` 件数・`exit code` に影響しない
- **受け入れ基準**:
  - Given ルール`KAL-PAT003`を `enabled = false` に設定, When 解析実行, Then `KAL-PAT003` の診断は報告されない
  - Given `KAL-F001` を `enabled = false` に設定, When 解析実行, Then `KAL-F001` の診断は報告されず、`KAL-F001` にバインドされたメトリクスは `scope_risk` 集約から除外される
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-027: メトリクス閾値のカスタマイズ

- **説明**: 設定ファイルでルールごとの閾値を変更できる
- **受け入れ基準**:
  - Given ルール`KAL-F001` の閾値を `0.55` から `0.60` に変更, When 解析実行, Then `0.60` を閾値として判定される
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-028: ファイル/ディレクトリ単位の除外パターン設定

- **説明**: 設定ファイルの `[general].exclude` フィールド（glob パターンの配列）により解析対象を除外する
- **受け入れ基準**:
  - Given `[general] exclude = ["generated/**"]` 設定, When 解析実行, Then generated配下のファイルは解析対象から除外される
- **優先度**: Must
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-006

#### REQ-FUNC-029: インラインコメントによる診断抑制

- **説明**: ソースコード中に `// kalos-ignore` または `# kalos-ignore`（言語に応じた形式）のコメントを記述することで、代表位置が一致する診断を抑制する。コメントが関数/class/module 宣言の直前行にあり、その間に空行や別コメントがない場合は、その宣言行を代表位置とする診断にも適用する。ルールID指定は exact match のみを許可する。cross-scope 診断の synthetic な代表位置（`start_line = 1`, `column = null`）にはインライン抑制を適用しない
- **受け入れ基準**:
  - Given 関数の直前行に `// kalos-ignore[KAL-F001]`, When 診断実行, Then 当該関数の `KAL-F001` 診断は報告されない
  - Given `// kalos-ignore`（ルールID指定なし）, When 診断実行, Then 対象行または直後スコープに結び付くすべての診断が抑制される
  - Given `KAL-PAT003` のような cross-scope 診断, When 代表位置ファイルの先頭へ `kalos-ignore` を書いても, Then 当該診断は抑制されず、ルール設定でのみ無効化できる
- **優先度**: Should
- **出典**: ユーザー確認済み

#### REQ-FUNC-030: デフォルト設定ファイルの生成

- **説明**: `kalos init` コマンドで、すべてのルールとデフォルト閾値を含む `.kalos.toml` を生成する
- **受け入れ基準**:
  - Given プロジェクトディレクトリ, When `kalos init` を実行, Then すべてのルールとデフォルト閾値・設定を含む `.kalos.toml` が生成される
  - Given `.kalos.toml` が既に存在 AND TTY stdin, When `kalos init` を実行, Then 上書き確認のプロンプトを表示する
  - Given `.kalos.toml` が既に存在 AND `--force` または `--yes`, When `kalos init` を実行, Then プロンプトを出さず上書きする
  - Given `.kalos.toml` が既に存在 AND 非TTY stdin AND `--force` 未指定, When `kalos init` を実行, Then プロンプトを出さず exit=2 で中断する
- **優先度**: Should
- **出典**: ユーザー確認済み

### 3.6 CI/CD統合

#### REQ-FUNC-031: 配布バイナリの提供

- **説明**: 以下のプラットフォーム向けにプリビルドバイナリを提供する
  - Linux: x86_64, aarch64
  - macOS: x86_64, aarch64
  - Windows: x86_64
- **配布契約**: CodeQL 管理対象 bundle の version/checksum を定義する managed bundle manifest は、kalos リリースの一部としてバイナリと一体で versioning される
- **受け入れ基準**:
  - Given 各対応プラットフォームのクリーン環境, When バイナリをダウンロードして `kalos check`（引数省略）を実行, Then kalos CLI 自身が CodeQL 管理対象 bundle の bootstrap / 検証 / キャッシュを行い、手動の追加ランタイムインストールなしで動作する
  - Given 同一 kalos リリースのバイナリ, When CodeQL bundle を bootstrap する, Then 利用する bundle の version/checksum はそのリリース同梱の managed bundle manifest によって一意に決まる
- **優先度**: Must
- **出典**: ユーザー確認済み + 2026-03-19 ユーザー判断

#### REQ-FUNC-032: GitHub Actions公式Actionの提供

- **説明**: GitHub Actionsワークフローから簡単に利用できる公式Actionを提供する
- **受け入れ基準**:
  - Given GitHub Actionsワークフロー, When 公式Actionを使用, Then `kalos` バイナリの取得、管理対象 CodeQL bundle と baseline cache の復元/保存、解析実行が自動で行われる
  - Given 公式Action経由でクリーンな runner が起動, When 解析実行, Then 実際の CodeQL bundle bootstrap / 検証は kalos CLI と同じ経路で行われ、Action は prewarm と cache orchestration の wrapper に留まる
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-033: SARIF出力によるGitHub Code Scanning連携

- **説明**: SARIF形式の出力をGitHub Code Scanningにアップロードし、PR上で診断結果を表示する
- **受け入れ基準**:
  - Given SARIF形式で出力された結果, When GitHub Code Scanningにアップロード, Then PR上に診断がアノテーションとして表示される
- **優先度**: Should
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-021

#### REQ-FUNC-034: 差分解析モード

- **説明**: git diff ベースで変更ファイルのみを再抽出し、互換なベースライン断片を再利用することで PR ごとの実行時間を短縮する
- **入力**: `--diff <base-ref>` オプション（例: `--diff HEAD~1`, `--diff main`）と、互換なベースラインキャッシュ（任意）
- **処理**:
  - 指定された `base-ref` からの変更ファイルを特定し、当該ファイルのみを再抽出する
  - 変更ファイルから逆依存閉包で `AffectedScopeSet` を求め、影響範囲のみ再計算する。diff 最適化が有効な実行では `Project` scope を常に再計算対象へ含め、project-level metrics と `scores.overall` / `scores.project` を stale な baseline 断片からそのまま流用してはならない
  - 互換なベースラインが存在する場合、非変更スコープの `ScopeMetrics` と `ScopeDiagnosticSnapshot` を再利用する。ベースラインの互換性は `BaselineFingerprint`（`workspace_root_hash`、`base_snapshot_hash`、`config_hash`、`analysis_targets_hash`、`rule_catalog_version`、`extractor_version`、`kalos_version`）の完全一致で判定する
  - `analysis_targets_hash` の正規化規則: 位置引数省略時（デフォルト）は正規形 `["."]` からハッシュを算出する。位置引数が明示的に指定された場合は、`WorkspaceRoot` 相対パスへ正規化し、ソート済み重複排除リストからハッシュを算出する（ADR-0003 参照）
  - プラグインメトリクスのベースライン再利用は、当該プラグインが現在の実行で正常にロード・評価された場合に限る。ロード失敗・fuel budget 超過・スキップされたプラグインの `MetricValue` は baseline 断片から除外する
  - ベースラインキャッシュは `--level` に関わらず以下の全構成要素を保存する（永続化ペイロード）: (1) 全階層の `ScopeMetrics`（丸め済み `scope_risk` を含む function / module / project）、(2) `ScopeDiagnosticSnapshot`（`primary_scope_id` ごとの診断断片）、(3) `OverallScore`（丸め済み `function_risk` / `module_risk` / `project_risk` / `overall_risk` と整数 `*_score`）、(4) `DependencyIndexManifest`（全スコープ間の依存辺）。`--level` は報告対象の制限であり、保存範囲には影響しない。これにより、異なる `--level` での実行間でもベースラインを再利用できる
  - baseline cache の永続化対象は全ワークスペース解析（`targets_explicitly_specified = false` の実行）に限定する。`targets_explicitly_specified = true`（明示指定）の実行は baseline を生成せず、既存 baseline も読み込まない。この場合 `--diff` 最適化は無効化し、要求された `analysis_targets` のみを対象とした non-diff 全スコープ解析へフォールバックする（全ワークスペースへの拡張は行わない）。`--level` は指定通り保持する
  - ベースラインが存在しない、互換でない、影響範囲を安全に確定できない、または project scope を安全に再計算できない場合は、要求された `analysis_targets` / `--level` を保った non-diff 全スコープ解析へフォールバックする
  - baseline cache の保存場所は環境変数 `$KALOS_CACHE_DIR` で指定する。未設定時のプラットフォーム別既定: Linux/macOS は `$XDG_CACHE_HOME/kalos` または `~/.cache/kalos`、Windows は `%LOCALAPPDATA%\kalos`（ADR-0003 参照）
  - baseline cache の再利用は best-effort とし、checkout path が変わる CI や cache 未復元環境では correctness を優先して全解析へフォールバックする
  - 差分モードの個別診断一覧は `AffectedScopeSet` に属するスコープのみを表示する
  - `--level all`（デフォルト）の場合、総合スコアと重大度別件数は変更後の解決済み `analysis_targets` 全体（`summary_scope = "whole_project"`）を母集団とし、機械可読出力では `diagnostics_scope = "affected_only"` かつ `summary_scope = "whole_project"` を必須とする
  - `--level function|module|project` の場合、重大度別件数と exit code は `AffectedScopeSet` 内の指定階層診断のみを母集団とし、機械可読出力では `diagnostics_scope = "affected_only"` かつ `summary_scope = "listed_diagnostics"` を必須とする。`scores.overall` は post-change 状態の指定階層メトリクスから算出した総合スコア（指定階層に計算可能なスコープが存在しない場合は `null`）、非対象階層の `scores.*` は `null` とする
  - フォールバック通知や bootstrap 通知などの運用メッセージは `stderr` にのみ出力し、`stdout` は要求された形式（human/json/sarif）を保つ
- **受け入れ基準**:
  - Given `--diff HEAD~1` と互換なベースライン, When 解析実行, Then 直前コミットからの変更ファイルのみが再抽出され、総合スコアは変更後の解決済み `analysis_targets` 全体値（merged post-change）として出力される
  - Given `--diff HEAD~1 --level function` と互換なベースライン, When 解析実行, Then 関数レベルの影響範囲診断のみが一覧に含まれ、機械可読出力の `summary_scope` は `"listed_diagnostics"` となる
  - Given `--diff HEAD~1 src/foo.rs` のように `analysis_targets` が部分集合, When 解析実行, Then baseline は read/write されず、要求された target 群に対する non-diff 全スコープ解析へフォールバックする
  - Given `--diff HEAD~1` だがベースラインが存在しない, When 解析実行, Then 全解析にフォールバックし、その旨が `stderr` に明示される
- **優先度**: Should
- **出典**: ユーザー確認済み + 2026-03-19 設計判断

## 4. 非機能要件

### 4.1 性能

#### REQ-NF-001: 中規模プロジェクトの全階層解析時間

- **基準**: 1万LOCのプロジェクトに対する全階層解析を60秒以内で完了する
- **測定条件**:
  - ベンチマークプロファイル `bench-linux-x64`（Linux x86_64, 4 vCPU, 16GB RAM, SSD）
  - `kalos` 本体と CodeQL 管理対象 bundle は事前に取得済み
  - `--llm` は無効（LLM は optional sidecar として別予算で扱う）
  - ソース checkout は cold、baseline cache は empty（全解析では未使用）
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-NF-002: 差分解析の実行時間

- **基準**: 10ファイル以下の差分解析を10秒以内で完了する
- **測定条件**:
  - `bench-linux-x64` プロファイルを使用
  - `--llm` は無効
  - CodeQL 管理対象 bundle は warm、baseline cache は warm
  - checkout path は stable で、`workspace_root_hash` が前回実行と一致する
  - 変更ファイル数は 10 以下、`base-ref` はローカルに解決可能
- **優先度**: Must
- **出典**: ユーザー確認済み

### 4.2 再現性

#### REQ-NF-003: 決定論的評価

- **基準**: 同一ソースコード・同一設定に対する評価結果（メトリクス値・診断・総合スコア）がビット単位で一致する。テンプレートベースの改善提案も決定論的とする
- **追加規約**: プラグインメトリクスの budget 制御は WASM fuel metering で行い、壁時間に依存させない（ADR-0004 参照）。diff mode では現在の実行で失敗またはスキップしたプラグインの baseline cache 済み `MetricValue` を出力へ持ち込まない
- **例外**: LLM連携モード（`--llm`）使用時の改善提案テキストは非決定論的となりうる。ただし、テンプレートベースの結果も併記するため、スコア・診断の決定論性は保たれる
- **優先度**: Must
- **出典**: ユーザー明示

### 4.3 可搬性

#### REQ-NF-004: 対応プラットフォーム

- **基準**: 以下のプラットフォームで動作する
  - Linux: x86_64, aarch64
  - macOS: x86_64, aarch64
  - Windows: x86_64
- **優先度**: Must
- **出典**: ユーザー確認済み

### 4.4 拡張性

#### REQ-NF-005: 言語サポートの追加

- **基準**: 新しい言語のサポートを追加する際、CPG抽出境界の内部にある言語パーサー、`UnifiedCpg` への変換、owner/public semantics を正規化する language profile の追加で対応可能な設計とする。メトリクス算出・スコア集約・レポート・CLI 等のコアコンポーネントへの変更を不要とする
- **完了条件の注記**: 本要件は CPG 抽出境界内の拡張性保証（parser / normalizer / language profile）を定める。新言語の完全なサポートには、これに加えて外部依存の型情報・シグネチャ解決のための language-specific resolver adapter（`REQ-FUNC-007`、`Dependency Symbol Resolver Port`）が必要となる。resolver adapter の各言語実装は 5 章 #3 の PoC 項目として別途追跡する（ADR-0002 §新言語追加時のスコープに関する注記 参照）
- **優先度**: Must
- **出典**: エージェント推測→ユーザー確認済み

#### REQ-NF-006: メトリクス定義の追加

- **基準**: v1 の report-only plugin metric を追加する際、`MetricDefinition` と算出ロジックの実装・登録だけで対応可能な設計とする。CPG抽出・CLI・設定管理等の既存コンポーネントへの変更を最小限に抑える。組み込みの scored metric を追加する場合の RuleId/診断契約拡張は v1.1 以降の設計対象とする
- **優先度**: Must
- **出典**: エージェント推測→ユーザー確認済み

### 4.5 ユーザビリティ

#### REQ-NF-007: ゼロコンフィグでの初回実行

- **基準**: 設定ファイルなしの状態で `kalos check`（引数省略）を実行するだけで、デフォルトのルール・閾値で解析結果を得られる
- **優先度**: Must
- **出典**: エージェント推測→ユーザー確認済み

### 4.6 LLM連携時の可用性

#### REQ-NF-008: LLMフォールバック

- **基準**: LLM連携モード使用時にLLMが応答しない、タイムアウトする、代表ファイルの言語を一意に解決できない、または multi-file / multi-language 診断の必須根拠を代表ファイル断片へ還元できない場合、テンプレートベースの改善提案にフォールバックする。kalos全体の動作がLLMの可用性に依存しない
- **優先度**: Must
- **出典**: ユーザー確認済み

### 4.7 外部通信とオフライン動作

#### REQ-NF-009: 外部通信の明示性と安全性

- **基準**:
  - ネットワーク通信は (a) kalos CLI が行う CodeQL 管理対象 bundle の bootstrap、(b) `--llm` 指定時の LLM 呼び出し、のみに限定する
  - CodeQL bundle 取得は固定バージョン + SHA-256 検証付きで行い、その正本は kalos リリースに同梱される managed bundle manifest とする
  - LLM の API キーは環境変数 `KALOS_LLM_API_KEY` のみから取得し、設定ファイルへ保存しない
  - LLM プロバイダは環境変数 `KALOS_LLM_PROVIDER` で指定する（v1 の許容値: `openai`）。未設定時のデフォルトは `openai` とする
  - LLM エンドポイント URL は環境変数 `KALOS_LLM_ENDPOINT_URL` で設定する。未設定時は `KALOS_LLM_PROVIDER` で決まるプロバイダ固有のデフォルト URL を使用する（例: `openai` → `https://api.openai.com/v1`）。接続先エンドポイント URL は info レベルでログ出力する（ペイロードは出力しない）
  - LLM outbound payload は allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, workspace_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }` のみを許可し、`language` は代表ファイルの `SourceAnalysis.source_files` メタデータから解決でき、かつ必須根拠を代表ファイル断片へ還元できた場合に限る。request ごとに `source_excerpt` と `cpg_excerpt` は相互排他的とする
  - リポジトリ全体、診断対象外の周辺コード、環境変数、シークレット、絶対パスは LLM に送信しない
  - **v1 ディスパッチ**: LLM 呼び出しは sequential（max in-flight = 1）とする。per-request: `connect timeout = 3s`, `overall timeout = 30s`。**ステータス別リトライ**: 429 は `Retry-After` ヘッダーを尊重して 1 回だけリトライする（aggregate budget 残量が許す場合のみ）。5xx はリトライせずスキップする。それ以外のエラーもリトライせずスキップする（ADR-0005 参照）
  - **Aggregate sidecar budget**: 1 回の `kalos check` 全体で LLM sidecar に費やす壁時間（wall-clock time）の上限は `120s`（暫定値）とする。v1 では sequential ディスパッチのため、各 request の所要時間（429 リトライの待機時間を含む）の累積で会計する。上限到達後は残りの `LlmEnrichmentRequest` をスキップし、テンプレート提案のみ返す。`stderr` / 構造化ログへ warning を出力する。暫定値は PoC で確定予定（ADR-0005 参照）
  - **Preflight failure**: `--llm` 指定時に `KALOS_LLM_API_KEY` が未設定の場合は設定エラー（exit code 2）とする。`KALOS_LLM_ENDPOINT_URL` が不正な URL 構文の場合も同様とする。`KALOS_LLM_PROVIDER` が v1 の許容値（`openai`）以外の値に設定されている場合も設定エラー（exit code 2）とする（ADR-0005 参照）。代表ファイルの言語解決不可・multi-file 診断の断片還元不可による request 省略は正常動作であり warning を出さない（ADR-0005 参照）
  - **URL 秘匿化**: エンドポイント URL のログ出力時はスキーム・ホスト・パスのみを記録し、クエリパラメータとフラグメントは除去する。URL に含まれうるトークンや API キーの資格情報漏えいを防ぐ（ADR-0005 参照）
- **優先度**: Must
- **出典**: 2026-03-19 ユーザー判断 + 設計具体化

#### REQ-NF-010: オフライン実行可能性

- **基準**:
  - CodeQL 管理対象 bundle がキャッシュ済みで `--llm` を使わない場合、ネットワーク未接続でも `kalos check` が成功する
  - bundle 未取得かつオフラインの場合は、bootstrap が必要であることを示す明確なエラーを出し exit code 2 で終了する
  - `--llm` 使用時にネットワーク障害またはタイムアウトが発生しても、コア診断・総合スコア・exit code は変化しない
- **優先度**: Must
- **出典**: 2026-03-19 ユーザー判断 + 設計具体化

## 5. PoC / 将来拡張で検証する項目

| # | 内容 | 関連要件 | 確認先 | 備考 |
|---|---|---|---|---|
| 1 | CodeQL 代替アダプタ比較を継続するか | REQ-FUNC-001〜004, REQ-NF-005 | PoC 完了後の ADR 見直し | v1 は CodeQL 既定 |
| 2 | WASM プラグイン SDK と配布パッケージ形式 | REQ-FUNC-012, REQ-NF-006 | v1.1 設計 | v1 の `plugin_manifest` は `.kalos.toml` 正規化結果で固定済み |
| 3 | 各言語の外部シンボル解決アダプタ実装 | REQ-FUNC-007 | 言語別設計ノート | lockfile / stub / local metadata を使ったローカル解決で、解析時ネットワーク不要を満たすこと |
| 4 | NF-001 の 60 秒目標と CodeQL 抽出時間の両立可能性 | REQ-NF-001 | ベンチマーク PoC | 未達なら代替アダプタを比較 |
| 5 | 新しい report-only plugin metric を `MetricDefinition` 実装と `.kalos.toml` 登録だけで差し込めるか | REQ-FUNC-012, REQ-NF-006 | 拡張性 PoC | 既存の CPG 抽出・CLI・設定管理を変更せずに成立することを確認 |

## 要件間の関連

### 派生関係（derives from）

- REQ-FUNC-013 → REQ-FUNC-016: 閾値違反の診断（013）から重大度付与（016）が派生
- REQ-FUNC-008〜010 → REQ-FUNC-011: 各階層メトリクス（008〜010）から総合スコア（011）が派生

### 依存関係（depends on）

- REQ-FUNC-008〜010 は REQ-FUNC-001〜004（CPG生成）に依存
- REQ-FUNC-013 は REQ-FUNC-008〜010（メトリクス算出）に依存
- REQ-FUNC-015 は REQ-FUNC-013（診断報告）, REQ-FUNC-001〜004（該当コード断片取得）, REQ-NF-008〜010（LLM可用性・外部通信・オフライン制約）に依存
- REQ-FUNC-011 は REQ-FUNC-008〜010 に依存
- REQ-FUNC-019, 020, 021 は REQ-FUNC-013, 015 に依存
- REQ-FUNC-022 は REQ-FUNC-016（重大度）に依存
- REQ-FUNC-033 は REQ-FUNC-021（SARIF出力）に依存
- REQ-FUNC-034 は REQ-FUNC-005（一括解析）, REQ-FUNC-031（管理対象 bundle）, REQ-NF-002（性能目標）に依存

### パイプライン依存チェーン

```
CPG抽出 (001-007) → メトリクス算出 (008-011) → 診断生成 (013-017) → 結果出力 (018-024)
                                                                           ↑
                                                        設定・ルール管理 (025-030)
```

## 変更履歴

| バージョン | 日付 | 変更内容 | 変更者 |
|---|---|---|---|
| 0.4.14 | 2026-03-27 | 変更履歴修正: v0.4.12 エントリの誤った REQ-ID 参照を訂正（REQ-FUNC-032 → REQ-FUNC-024。scores nullability は総合スコアサマリー要件に関連） | Claude |
| 0.4.13 | 2026-03-27 | provenance 整備: レビュー者メタ情報にレビュー対象版・日付を追記、`SummaryScope::WholeProject` 表記を domain_model.md の dot 表記（`SummaryScope.WholeProject`）に統一 | Claude |
| 0.4.12 | 2026-03-27 | `scores` nullability 契約の統一: `scores.overall` / `scores.function` / `scores.module` / `scores.project` の `null` 条件を「非対象階層」と「計算可能なスコープ不在」の 2 源に明確化し domain_model.md と整合（REQ-FUNC-011 ステップ 7/8、REQ-FUNC-020、REQ-FUNC-023、REQ-FUNC-024） | Claude |
| 0.4.11 | 2026-03-27 | レビュー findings 解決: REQ-FUNC-012 の normative ABI 参照リストに §SPI v1 列挙契約を追加（ADR-0004 のフィルタ済みカウント/インデックス空間・再番号付けセマンティクスへのトレーサビリティ確保） | Claude |
| 0.4.10 | 2026-03-27 | レビュー findings 解決: REQ-FUNC-023 の `--level` 内部動作を「追加で算出してよい」から「常に全階層を算出する」に強化し、Reporting が射影 owner と明記（ADR-0003 保存不変条件との整合） | Claude |
| 0.4.9 | 2026-03-27 | レビュー findings 解決: REQ-NF-008〜010 の依存ラベルを「LLM可用性・外部通信・オフライン制約」に修正（LLM 限定表現の是正） | Claude |
| 0.4.8 | 2026-03-27 | レビュー findings 解決: `full mode` を `non-diff モード` に統一（ADR-0003 の用語区別に整合）、`変更後プロジェクト全体` を `解決済み analysis_targets 内の全階層` に明確化 | Claude |
| 0.4.7 | 2026-03-26 | レビュー findings 解決: REQ-FUNC-018 の `--llm` に full/diff 両モード動作とエンリッチ対象スコープを明記 | Claude |
| 0.4.6 | 2026-03-26 | レビュー指摘解決: invalid-value contract に `raw_value` の NaN/Inf 検査を追加（ADR-0004・domain_model.md と同期） | Claude |
| 0.4.5 | 2026-03-22 | ADR-0004 ABI 明確化に伴う同期: REQ-FUNC-012 の normative ABI 参照リストを更新（ScopeId 直列化契約、線形メモリデータレイアウト、スカラー戻り値エンコーディングの用語分離を反映） | Claude |
| 0.4.4 | 2026-03-22 | レビュー findings 解決: REQ-FUNC-012 にSPI v1 ABI normative 参照（ADR-0004）を追加、REQ-NF-009 Preflight failure に unsupported `KALOS_LLM_PROVIDER` の設定エラー（exit code 2）を追加（ADR-0005） | Claude |
| 0.4.3 | 2026-03-22 | レビュー findings 解決: non-diff full mode のベースライン動作を write-back only に明確化、`targets_explicitly_specified: bool` による CLI path 引数の由来記録を追加、baseline 永続化判定に `targets_explicitly_specified` を使用 | Claude |
| 0.4.2 | 2026-03-22 | レビュー findings 解決: 校正注記の provenance 修正（unsupported 出典主張を除去）、`kalos check .`（明示 `.`）と引数省略の scope semantics を整合、REQ-NF-007・REQ-FUNC-031 の例示を引数省略形に統一 | Claude |
| 0.4.1 | 2026-03-22 | レビュー指摘解決: `REQ-NF-005` に resolver adapter（`REQ-FUNC-007`）との関係を完了条件注記として追加 | Claude |
| 0.4.0 | 2026-03-22 | 再レビュー指摘解決: 版メタ v0.4.0 同期、`normalized_risk` の `NaN`/`Inf`/out-of-range セマンティクス追加、aggregate fuel budget の diff→全解析フォールバック規約追加 | Claude |
| 0.3.1 | 2026-03-22 | REQ-NF-009 に ADR-0005 の LLM runtime policy（aggregate sidecar budget 120s、preflight failure、URL 秘匿化契約）を伝播 | Claude |
| 0.3.0 | 2026-03-21 | レビュー指摘解決: `enabled = false` セマンティクスの明文化（REQ-FUNC-026 拡充、REQ-FUNC-011 scope_risk 除外注記）、KAL-PAT002 受け入れ基準追加、summary_scope 表記を snake_case に統一、デフォルト閾値の校正注記追加、subset analysis_targets フォールバックの明確化、用語集にアーキテクチャコンポーネント定義を追加 | Claude |
| 0.2.12 | 2026-03-20 | `primary_scope_id` による診断の canonical scope 契約、Application Pipeline による summary materialization、plugin baseline 再利用ゲートと `aggregate_fuel_budget` による決定論性規約を追加 | Codex |
| 0.2.11 | 2026-03-19 | `Diagnostic.location` のフィールド名を `start_line`/`end_line`/`column` に統一、full mode の診断完全性を「選択された --level に関して完全」へ明確化、plugin の level-to-subgraph 契約と `schema_version` 初期値 `"1.0.0"` / バンプポリシーを定義 | Claude |
| 0.2.10 | 2026-03-19 | `kalos check` の位置引数省略時デフォルト `.` を明記、`analysis_targets` の正規化・検証責務を Configuration に一本化、SARIF の rule/severity/location 写像を拡充、stray `API` 表記を除去、メタ情報バージョンを同期 | Claude |
| 0.2.9 | 2026-03-19 | 明示 `--config` の `WorkspaceRoot` 契約、`analysis_targets` 正規化基準、diff fallback 条件のトレーサビリティを補強 | Codex |
| 0.2.8 | 2026-03-19 | `scores.overall` の metrics 起源、summary との責務分離、plugin checksum 構文検証を明文化 | Codex |
| 0.2.7 | 2026-03-19 | plugin `metric_id` 衝突契約、cross-scope 診断の表示/抑制規則、SARIF 写像の固定を明文化 | Codex |
| 0.2.6 | 2026-03-19 | score.weights/threshold の検証規則、KAL-PAT001 の --level module 動作、ベースライン互換性（analysis_targets_hash・全階層保存）を明文化 | Claude |
| 0.2.5 | 2026-03-19 | 外部シンボル解決のローカル入力契約、managed bundle manifest の正本、配布契約を明文化 | Codex |
| 0.2.4 | 2026-03-19 | 複数 target の CLI/JSON 契約、metric severity override、plugin extensibility PoC 項目を明文化 | Codex |
| 0.2.3 | 2026-03-19 | plugin aggregate budget を Metrics stage 内数へ調整し、LLM excerpt one-of 契約を明文化 | Codex |
| 0.2.2 | 2026-03-19 | WorkspaceRoot/`workspace_relative_path` 契約、Go owner scope=package、Rust semantic edge、plugin report-only 契約、PAT001 Python 規則を明文化 | Codex |
| 0.2.1 | 2026-03-19 | PAT001 粒度、`--strict`、`exclude` マージ、plugin manifest、LLM representative file 契約を明文化 | Codex |
| 0.2.0 | 2026-03-19 | メトリクス数式・総合スコア集約・重大度境界・差分解析契約・CodeQL 自動取得・LLM 入力制約を確定 | Codex |
| 0.1.0 | 2026-03-18 | 初版作成 | Claude（requirements-definer スキル） |
