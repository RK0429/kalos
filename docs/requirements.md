# kalos 要件定義書

## メタ情報

| 項目 | 内容 |
|---|---|
| バージョン | 0.2.0 |
| 最終更新日 | 2026-03-19 |
| ステータス | ドラフト |
| 作成者 | Claude（requirements-definer スキル） |
| レビュー者 | Codex |

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
| CPG抽出エンジン | CodeQLを前提とする。4言語対応かつ長期安定の代替（Joern, Tree-sitter等）があれば設計フェーズで比較検討する |
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
| ルール | 特定のメトリクスまたはパターンと閾値、重大度、提案テンプレートの組み合わせ。一意のルールID（`KAL-F001`, `KAL-M001`, `KAL-P001`, `KAL-PAT001` 形式）で識別される |
| 総合スコア | 各階層の正規化リスク値（0.0〜1.0, 高いほど悪い）を重み付き集約し、`100 * (1 - overall_risk)` で算出する品質スコア |
| 統一CPG表現 | 4言語のCPGを言語非依存な共通構造と言語固有の拡張ノードで表現する内部データ構造 |
| CodeQL | GitHub が開発するコード解析エンジン。ソースコードをデータベース化し、クエリ言語でコードプロパティを検索・抽出できる |
| SARIF | Static Analysis Results Interchange Format。静的解析ツールの結果を表現するJSON形式の標準規格（OASIS標準） |

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

- **説明**: Rust ソースファイルを解析し、統一CPG表現に変換する。所有権・借用・ライフタイム等のRust固有概念を拡張ノードとして保持する
- **入力**: Rust ソースファイル（`.rs`）
- **処理**: REQ-FUNC-001 と同様の処理フロー。Rust固有の所有権モデルを拡張ノードとして保持する
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
- **入力**: glob パターン（CLI引数 `--exclude` または設定ファイル）
- **処理**: 指定パターンにマッチするファイルを解析対象から除外する。`.gitignore` が存在する場合、そのパターンをデフォルトで除外対象とする
- **受け入れ基準**:
  - Given `--exclude "vendor/**"` 指定, When 解析実行, Then vendor配下のファイルは解析対象から除外される
  - Given `.gitignore` が存在し除外指定なし, When 解析実行, Then `.gitignore` のパターンに該当するファイルは除外される
- **優先度**: Must
- **出典**: ユーザー確認済み（`.gitignore` デフォルト除外はエージェント推測→ユーザー確認済み）

#### REQ-FUNC-007: 外部依存の型情報・シグネチャ解決

- **説明**: 外部ライブラリの型情報・関数シグネチャを解決し、CPGのモジュール間エッジの精度を確保する
- **入力**: プロジェクトの依存関係定義（`requirements.txt`, `package.json`, `Cargo.toml`, `go.mod` 等）
- **処理**: 各言語のパッケージマネージャまたは型スタブを利用し、外部依存の公開API（関数シグネチャ、型定義）を取得する。取得した情報をCPGの外部ノードとして統合する
- **例外**: 型情報が取得できない依存については、解決失敗として警告を出力する。メトリクス算出時には解決済みの依存のみで精度の範囲内の評価を行う
- **受け入れ基準**:
  - Given `Cargo.toml` に記載された外部クレート, When CPG生成を実行, Then 当該クレートの公開関数シグネチャがCPGの外部ノードとして含まれる
  - Given 型情報が取得できない依存, When CPG生成を実行, Then 解決失敗の警告を出力する
- **優先度**: Should
- **出典**: ユーザー明示（「unknownは投げやりなので、きちんと解決すること」）
- **関連要件**: REQ-FUNC-001〜004

### 3.2 メトリクス算出

v1 では、すべてのメトリクスを `raw_value` と `normalized_risk` の組で保持する。`normalized_risk` は `0.0〜1.0` の閉区間に正規化されたリスク値であり、`0.0` が最良、`1.0` が最悪を表す。`H` は底 2 の Shannon entropy、`clamp(x, 0, 1)` は 0 未満を 0、1 超を 1 に丸める操作とする。`raw_value`, `normalized_risk`, `scope_risk`, `level_risk`, `overall_risk`, `overflow_ratio` は、それぞれ算出直後に小数第 6 位で round-half-up し、その丸め済み値をキャッシュ・比較・外部出力に用いる。

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

#### REQ-FUNC-009: モジュール/ファイルレベルメトリクスの算出

- **説明**: CPG のモジュール/ファイルサブグラフに対して、v1 で固定したモジュールレベルメトリクスを算出する
- **入力**: 統一CPGのモジュールサブグラフ
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
  1. 各スコープの `scope_risk` を、そのスコープに属する `normalized_risk` の算術平均として算出する
  2. 各階層の `level_risk` を、その階層に属する `scope_risk` の算術平均として算出する。プロジェクト階層は単一スコープなので、その `scope_risk` をそのまま用いる
  3. デフォルト重みは `function: 0.4`, `module: 0.35`, `project: 0.25` とし、設定ファイルで上書き可能とする
  4. ある階層にスコープが 0 件の場合、その階層の重みは残る階層へ比例再配分する
  5. `scope_risk`, `level_risk`, `overall_risk` は各段階の算出直後に小数第 6 位で round-half-up し、その値をキャッシュと後続計算に用いる
  6. `overall_risk = Σ(adjusted_weight[level] * level_risk[level])`
  7. `function_score`, `module_score`, `project_score`, `overall_score` はそれぞれ `round_half_up(100 * (1 - risk))` で整数化する
  8. `--level function|module|project` により非対象階層が未計算の場合、対応する `*_risk` / `*_score` は省略可能とし、機械可読出力では `null` に写像する

- **出力**: 総合スコア（0〜100 の整数）および各階層の部分スコア
- **受け入れ基準**:
  - Given 全階層のメトリクス結果, When 総合スコアを算出, Then 同一入力から常に同一の総合スコアと各階層スコアが出力される
  - Given 設定ファイルで重みを変更, When 総合スコアを算出, Then 変更後の重みと再配分規則で集約される
- **優先度**: Must
- **出典**: ユーザー確認済み + 2026-03-19 設計判断
- **関連要件**: REQ-FUNC-008, REQ-FUNC-009, REQ-FUNC-010

#### REQ-FUNC-012: メトリクス定義のプラグイン拡張

- **説明**: ユーザーが独自のメトリクス定義を追加できる拡張機構を提供する
- **入力**: プラグイン仕様に準拠したメトリクス定義
- **処理**: プラグインとして登録されたメトリクスを、組み込みメトリクスと同じパイプラインで算出する。Plugin Host は v1 の既定として invocation ごとに `cpu_time_budget = 50ms`、`linear_memory_limit = 64MiB` を適用し、ネットワーク・ファイル書込を禁止する
- **受け入れ基準**:
  - Given プラグイン仕様に準拠したメトリクス定義, When 解析実行, Then 当該メトリクスが組み込みメトリクスと同様に算出・報告される
  - Given プラグインが既定上限を超過, When 解析実行, Then 当該プラグイン評価は失敗として打ち切られ、kalos 本体の実行は継続する
- **優先度**: Should
- **出典**: ユーザー確認済み（当初Couldだったが、ユーザーの要望でShouldに昇格）

### 3.3 診断・改善提案

#### REQ-FUNC-013: メトリクス閾値違反の診断報告

- **説明**: 各メトリクスの `normalized_risk` をルールごとの閾値と比較し、違反をメトリクス診断として報告する
- **入力**: メトリクス算出結果、ルールごとの閾値設定
- **処理**: 各メトリクスの `normalized_risk` を閾値と比較し、超過があれば `kind = "metric"` の診断オブジェクトを生成する
- **出力**: メトリクス診断オブジェクトのリスト。各診断は共通フィールド `rule_id`, `severity`, `location`, `message`, `template_suggestion` と、`metric` フィールド `{ metric_id, raw_value, normalized_risk, threshold, overflow_ratio }` を持つ。単一ファイルへ結び付かない cross-scope 診断では、`location` は根拠 scope 群のうち辞書順最小 `file_path` の `line = 1`, `end_line = 1`, `column = null` を代表位置として用いる
- **受け入れ基準**:
  - Given 関数のCFGエントロピーが閾値を超過, When 診断実行, Then 当該関数の位置・ルールID・重大度・メトリクス値・閾値を含む診断が報告される
  - Given すべてのメトリクスが閾値内, When 診断実行, Then 診断は0件で正常終了する
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-014: 構造的パターンの検出

- **説明**: CPG から v1 で固定したソフトウェア設計上の問題パターンを検出する
- **入力**: 統一CPG
- **処理**: v1 では以下のパターンルールを適用する

  | RuleId | パターン | 対象 | 検出条件 | デフォルト重大度 |
  |---|---|---|---|---|
  | `KAL-PAT001` | God Unit | Python/TypeScript は class、Rust/Go は module/file | 対象ユニットが `public_member_count >= 20` かつ `fan_out >= 8` かつ配下関数の `M-F002` 平均 `>= 0.50` | `warning` |
  | `KAL-PAT002` | Feature Envy | 関数 | 外部オブジェクト/モジュールへの参照数が 5 以上かつ `foreign_accesses / (foreign_accesses + local_accesses) >= 0.70` | `warning` |
  | `KAL-PAT003` | Circular Dependency | モジュール依存グラフ | SCC のサイズが 2 以上 | `error` |

- **言語別の計数規則**:
  - `public_member_count` は Python/TypeScript では対象 class の public メソッド・public フィールド数（constructor, private, protected を除く）、Rust では対象 module/file 直下の `pub` な top-level item 数、Go では対象 package/file の exported top-level declaration 数とする
  - `foreign_accesses` は「現在の関数が所属する owner（class / module / package）以外」への参照・呼び出し数、`local_accesses` は同一 owner 内への参照・呼び出し数とする。Python/TypeScript の `self` / `this`、Rust の `self` / `Self` / 同一 module 内 item、Go の同一 receiver type または同一 package の識別子参照は local に数え、import 先 package や別 receiver type への selector / call は foreign に数える
- **出力**: `kind = "pattern"` の診断オブジェクトのリスト。各診断は共通フィールド `rule_id`, `severity`, `location`, `message`, `template_suggestion` に加え、`pattern` フィールド `{ pattern_type, evidence_scopes, evidence_message }` を持つ。単一ファイルへ結び付かない cross-scope 診断では、`location` は `evidence_scopes` のうち辞書順最小 `file_path` の `line = 1`, `end_line = 1`, `column = null` を代表位置として用いる
- **受け入れ基準**:
  - Given 過度に多くの責務を持つ class または module, When 診断実行, Then `KAL-PAT001` として検出される
  - Given モジュール依存グラフに循環がある, When 診断実行, Then `KAL-PAT003` として検出される
- **優先度**: Should
- **出典**: ユーザー確認済み + 2026-03-19 設計判断

#### REQ-FUNC-015: 具体的な改善提案テキストの生成

- **説明**: 各診断に対して、何が問題か・なぜ問題か・どう改善すべきかを含む具体的な改善提案テキストを生成する。テンプレートベースの生成を基本とし、オプションでLLM連携による文脈に即した提案生成を提供する
- **入力**: Application Pipeline が `Diagnostic` と `SourceAnalysis` から組み立てた allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, repo_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }`。`rule_id`, `severity`, `repo_relative_path` は `Diagnostic` から、`language` は `SourceAnalysis` から、`source_excerpt` / `cpg_excerpt` は対象スコープの CPG・ソースから取得する。`metric` と `pattern` は `Diagnostic.kind` に応じて排他的に設定される
- **処理**:
  - テンプレートモード（デフォルト）: 違反パターンごとの定型テンプレートにコード文脈を埋め込んで提案文を生成する
  - LLM連携モード（`--llm` オプション）: Application Pipeline は `Diagnostic` と `SourceAnalysis` から allowlist 済み `LlmEnrichmentRequest` を組み立てて LLM に渡す。許可するのは `rule_id`, `severity`, `language`, `repo_relative_path`, `metric` または `pattern`, `source_excerpt` または正規化済み `cpg_excerpt` のみとし、それ以外の診断内部情報は送信しない。テンプレートベースの結果も併記する。LLM非応答時はテンプレート結果にフォールバックする
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
  - `overflow_ratio < 0.25` なら `info`、`0.25 <= overflow_ratio < 0.60` なら `warning`、`0.60 <= overflow_ratio` なら `error`
  - パターン診断では `KAL-PAT001 = warning`, `KAL-PAT002 = warning`, `KAL-PAT003 = error` をデフォルトとし、設定ファイルで上書き可能とする
- **重大度定義**:
  - error: プロジェクトの品質基準を明確に逸脱（CI/CDでfailの根拠になる）
  - warning: 改善が強く推奨される
  - info: 改善の余地があるが許容範囲内
- **受け入れ基準**:
  - Given `overflow_ratio >= 0.60`, When 重大度判定, Then `error` が付与される
  - Given `0.25 <= overflow_ratio < 0.60`, When 重大度判定, Then `warning` が付与される
  - Given `0 < overflow_ratio < 0.25`, When 重大度判定, Then `info` が付与される
  - Given `KAL-PAT003`, When 重大度判定, Then デフォルトで `error` が付与される
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

- **説明**: `kalos check <path>` で対象パスの解析を実行する。CPG抽出→メトリクス算出→診断生成→結果出力の全パイプラインを統合する
- **入力**: 解析対象パス（ファイルまたはディレクトリ）、オプション引数
- **一覧・summary・exit code の母集団**:
  - 診断一覧: full mode では全診断、diff mode では `AffectedScopeSet` に属する診断のみ
  - `--severity` は一覧の表示/出力対象だけを絞り込み、summary と exit code の計算母集団は変えない
  - `--level all`（デフォルト）では、summary と exit code は「変更後プロジェクト全体」の診断集合を母集団とする
  - `--level <function|module|project>` 指定時は、指定階層の診断のみを母集団とする（REQ-FUNC-023 参照）
- **主要オプション**:
  - `--format <human|json|sarif>`: 出力形式（デフォルト: human）
  - `--level <function|module|project|all>`: 解析階層（デフォルト: all）
  - `--config <path>`: 設定ファイルパス
  - `--exclude <pattern>`: 除外パターン
  - `--severity <error|warning|info>`: 表示する最低重大度
  - `--diff <base-ref>`: 変更ファイル再抽出 + ベースライン再利用による差分解析
  - `--llm`: LLM連携による改善提案を有効化
  - `--strict`: warningをerror扱いとする
- **受け入れ基準**:
  - Given 有効なプロジェクトディレクトリ, When `kalos check .` を実行, Then 全対応言語ファイルが解析され、診断結果が端末に表示される
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

  src/lib.rs:1:1  error[KAL-PAT003]  [pattern] モジュール間に循環依存が存在する
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
  - Given `--level all`（デフォルト）で解析完了, When human形式で出力, Then 末尾に変更後プロジェクト全体の総合スコアサマリーと重大度別件数が表示される
  - Given `--level function` で解析完了, When human形式で出力, Then 末尾に関数レベル診断のみを母集団とした総合スコアサマリーと重大度別件数が表示され、module/project のスコアは表示されない
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-020: JSON形式での結果出力

- **説明**: 解析結果を機械可読なJSON構造で出力する。全メトリクス・総合スコア・summary を含み、`diagnostics` は full mode では選択された `--level` に対する完全な診断集合、diff mode では `AffectedScopeSet` に属する診断部分集合を返す。`schema_version` を持つ
- **最低限のJSON契約**:
  - ルートには `schema_version`, `analysis_target`, `scores`, `metrics`, `diagnostics`, `diagnostics_scope`, `summary`, `summary_scope`, `tool_version` を必須とする
  - `diagnostics[*]` は `kind` を discriminant とし、`kind = "metric"` なら `metric` オブジェクト、`kind = "pattern"` なら `pattern` オブジェクトを必須とする
  - `diagnostics[*].template_suggestion` は必須、`diagnostics[*].llm_suggestion` は任意とする
  - `scores` には `overall`, `function`, `module`, `project` を必須とする。`overall` は常に 0〜100 の整数、`function` / `module` / `project` は対象階層なら 0〜100 の整数、非対象階層なら `null` とする
  - `diagnostics_scope` は `whole_project | affected_only`、`summary_scope` は `whole_project | listed_diagnostics` とする
- **受け入れ基準**:
  - Given 解析結果, When `--format json` で出力, Then 出力が有効なJSONであり、上記の必須フィールドがすべて存在する
  - Given `--level all` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "whole_project"` かつ `summary_scope = "whole_project"` となり、`scores.function/module/project` はすべて整数となる
  - Given `--level function` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "whole_project"` かつ `summary_scope = "listed_diagnostics"` となり、`scores.overall` と `scores.function` は整数、`scores.module` と `scores.project` は `null` となる
  - Given `--diff <base-ref> --level all` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "affected_only"` かつ `summary_scope = "whole_project"` となる
  - Given `--diff <base-ref> --level function` かつ `--format json`, When 解析結果を出力, Then `diagnostics_scope = "affected_only"` かつ `summary_scope = "listed_diagnostics"` となり、`scores.overall` と `scores.function` は整数、`scores.module` と `scores.project` は `null` となる
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-021: SARIF形式での結果出力

- **説明**: 解析結果をSARIF（Static Analysis Results Interchange Format）形式で出力し、GitHub Code Scanning等との連携を可能にする
- **SARIF への写像方針**:
  - `template_suggestion` は `result.message.text` または `help` へ格納する
  - `llm_suggestion` が存在する場合は `result.properties.kalos.llm_suggestion` に格納する
- **受け入れ基準**:
  - Given 解析結果, When `--format sarif` で出力, Then 出力がSARIF 2.1.0スキーマに準拠する
- **優先度**: Should
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-020

#### REQ-FUNC-022: Exit codeによるパイプライン制御

- **説明**: 解析結果に応じたexit codeを返し、CI/CDパイプラインでのpass/fail判定を可能にする
- **判定母集団**: `--level all`（デフォルト）では exit code は変更後プロジェクト全体の診断集合を基準とし、`--severity` による表示フィルタの影響を受けない。`--level` で階層を限定した場合は指定階層の診断を基準とする（REQ-FUNC-023）。diff mode でも同様とする

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

- **説明**: `--level` オプションで解析対象の階層を限定する。CLI Shell がオプションを解釈し、Application Pipeline が指定階層のメトリクス算出・診断生成のみを実行する。CPG 抽出は全ファイルを対象とする（階層横断の依存解決に必要なため）
- **パイプライン動作**:
  - `--level all`（デフォルト）: 全階層のメトリクス・診断を算出し、総合スコアを報告する。`summary_scope = WholeProject`
  - `--level function|module|project`: 指定階層のメトリクス・診断のみを算出・報告する。総合スコアは指定階層の `level_risk` から算出する。機械可読出力では `scores.overall` をその総合スコアとし、非対象階層の `scores.*` は `null` とする。`summary_scope = ListedDiagnostics`
- **受け入れ基準**:
  - Given `--level function` 指定, When 解析実行, Then 関数レベルのメトリクスと診断のみが出力される
  - Given `--level function` かつ `--format json`, When 解析実行, Then `summary_scope = "listed_diagnostics"` となり、`scores.module` と `scores.project` は `null` となる
- **優先度**: Should
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-018, REQ-FUNC-020

#### REQ-FUNC-024: 総合スコアサマリーの表示

- **説明**: 解析結果の末尾に総合スコア・各階層スコア・重大度別件数のサマリーを表示する
- **サマリー母集団**: `--level all`（デフォルト）では summary は変更後プロジェクト全体の診断集合を基準とし、`--severity` による表示フィルタの影響を受けない。`--level` で階層を限定した場合は指定階層の診断を基準とする（REQ-FUNC-023）
- **受け入れ基準**:
  - Given `--level all` で解析完了, When 結果を出力, Then 総合スコア（0〜100）・各階層スコア・重大度別診断件数が表示される
  - Given `--level function` で解析完了, When 結果を出力, Then 総合スコア・関数階層スコア・重大度別診断件数が表示され、module/project のスコアは表示されない
- **優先度**: Must
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-011

### 3.5 設定・ルール管理

#### REQ-FUNC-025: プロジェクト設定ファイルの読み込み

- **説明**: `.kalos.toml` からルール・閾値・除外パターン・スコア重みの設定を読み込む。カレントディレクトリから親方向に設定ファイルを探索する（monorepo対応）
- **設定の優先順位**: CLI引数 > プロジェクト設定ファイル > デフォルト値
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
  ```
- **受け入れ基準**:
  - Given `.kalos.toml` が存在, When 解析実行, Then 設定ファイルの内容がルール・閾値に反映される
  - Given CLI引数と設定ファイルが競合, When 解析実行, Then CLI引数が優先される
  - Given 設定ファイルに構文エラー, When 解析実行, Then エラーメッセージと該当箇所を表示し exit code 2で終了する
  - Given 設定ファイルなし, When 解析実行, Then デフォルト値で動作する
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-026: 個別ルールの有効/無効切り替え

- **説明**: 設定ファイルで個別ルールの `enabled` を `false` に設定することで、当該ルールの診断を無効化できる
- **受け入れ基準**:
  - Given ルール`KAL-PAT003`を `enabled = false` に設定, When 解析実行, Then `KAL-PAT003` の診断は報告されない
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-027: メトリクス閾値のカスタマイズ

- **説明**: 設定ファイルでルールごとの閾値を変更できる
- **受け入れ基準**:
  - Given ルール`KAL-F001` の閾値を `0.55` から `0.60` に変更, When 解析実行, Then `0.60` を閾値として判定される
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-FUNC-028: ファイル/ディレクトリ単位の除外パターン設定

- **説明**: 設定ファイルの `exclude` フィールドでglob パターンにより解析対象を除外する
- **受け入れ基準**:
  - Given `exclude = ["generated/**"]` 設定, When 解析実行, Then generated配下のファイルは解析対象から除外される
- **優先度**: Must
- **出典**: ユーザー確認済み
- **関連要件**: REQ-FUNC-006

#### REQ-FUNC-029: インラインコメントによる診断抑制

- **説明**: ソースコード中に `// kalos-ignore` または `# kalos-ignore`（言語に応じた形式）のコメントを記述することで、代表位置が一致する診断を抑制する。コメントが関数/class/module 宣言の直前行にあり、その間に空行や別コメントがない場合は、その宣言行を代表位置とする診断にも適用する。ルールID指定は exact match のみを許可する
- **受け入れ基準**:
  - Given 関数の直前行に `// kalos-ignore[KAL-F001]`, When 診断実行, Then 当該関数の `KAL-F001` 診断は報告されない
  - Given `// kalos-ignore`（ルールID指定なし）, When 診断実行, Then 対象行または直後スコープに結び付くすべての診断が抑制される
- **優先度**: Should
- **出典**: ユーザー確認済み

#### REQ-FUNC-030: デフォルト設定ファイルの生成

- **説明**: `kalos init` コマンドで、すべてのルールとデフォルト閾値を含む `.kalos.toml` を生成する
- **受け入れ基準**:
  - Given プロジェクトディレクトリ, When `kalos init` を実行, Then すべてのルールとデフォルト閾値・設定を含む `.kalos.toml` が生成される
  - Given `.kalos.toml` が既に存在, When `kalos init` を実行, Then 上書き確認のプロンプトを表示する
- **優先度**: Should
- **出典**: ユーザー確認済み

### 3.6 CI/CD統合

#### REQ-FUNC-031: 配布バイナリの提供

- **説明**: 以下のプラットフォーム向けにプリビルドバイナリを提供する
  - Linux: x86_64, aarch64
  - macOS: x86_64, aarch64
  - Windows: x86_64
- **受け入れ基準**:
  - Given 各対応プラットフォームのクリーン環境, When バイナリをダウンロードして `kalos check .` を実行, Then CodeQL が未配置でも必要な管理対象 bundle が自動取得・検証・キャッシュされ、手動の追加ランタイムインストールなしで動作する
- **優先度**: Must
- **出典**: ユーザー確認済み + 2026-03-19 ユーザー判断

#### REQ-FUNC-032: GitHub Actions公式Actionの提供

- **説明**: GitHub Actionsワークフローから簡単に利用できる公式Actionを提供する
- **受け入れ基準**:
  - Given GitHub Actionsワークフロー, When 公式Actionを使用, Then `kalos` バイナリの取得・管理対象 CodeQL bundle のキャッシュ復元/取得・解析実行が自動で行われる
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
  - 変更ファイルから逆依存閉包で `AffectedScopeSet` を求め、影響範囲のみ再計算する
  - 互換なベースラインが存在する場合、非変更スコープの `ScopeMetrics` と `ScopeDiagnosticSnapshot` を再利用する
  - ベースラインが存在しない、互換でない、または影響範囲を安全に確定できない場合は全解析へフォールバックする
  - 差分モードの個別診断一覧は `AffectedScopeSet` に属するスコープのみを表示する
  - `--level all`（デフォルト）の場合、総合スコアと重大度別件数は「変更後のプロジェクト全体」を意味し、機械可読出力では `diagnostics_scope = "affected_only"` かつ `summary_scope = "whole_project"` を必須とする
  - `--level function|module|project` の場合、総合スコアと重大度別件数は `AffectedScopeSet` 内の指定階層診断のみを母集団とし、機械可読出力では `diagnostics_scope = "affected_only"` かつ `summary_scope = "listed_diagnostics"` を必須とする。`scores.overall` は指定階層の総合スコア、非対象階層の `scores.*` は `null` とする
  - フォールバック通知や bootstrap 通知などの運用メッセージは `stderr` にのみ出力し、`stdout` は要求された形式（human/json/sarif）を保つ
- **受け入れ基準**:
  - Given `--diff HEAD~1` と互換なベースライン, When 解析実行, Then 直前コミットからの変更ファイルのみが再抽出され、総合スコアは変更後のプロジェクト全体値として出力される
  - Given `--diff HEAD~1 --level function` と互換なベースライン, When 解析実行, Then 関数レベルの影響範囲診断のみが一覧に含まれ、機械可読出力の `summary_scope` は `"listed_diagnostics"` となる
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
  - ソース checkout は cold、baseline cache は empty（全解析では未使用）
- **優先度**: Must
- **出典**: ユーザー確認済み

#### REQ-NF-002: 差分解析の実行時間

- **基準**: 10ファイル以下の差分解析を10秒以内で完了する
- **測定条件**:
  - `bench-linux-x64` プロファイルを使用
  - CodeQL 管理対象 bundle は warm、baseline cache は warm
  - 変更ファイル数は 10 以下、`base-ref` はローカルに解決可能
- **優先度**: Must
- **出典**: ユーザー確認済み

### 4.2 再現性

#### REQ-NF-003: 決定論的評価

- **基準**: 同一ソースコード・同一設定に対する評価結果（メトリクス値・診断・総合スコア）がビット単位で一致する。テンプレートベースの改善提案も決定論的とする
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

- **基準**: 新しい言語のサポートを追加する際、言語パーサーと統一CPG表現への変換ロジックの実装のみで対応可能な設計とする。メトリクス算出・診断・CLI等の既存コンポーネントへの変更を不要とする
- **優先度**: Must
- **出典**: エージェント推測→ユーザー確認済み

#### REQ-NF-006: メトリクス定義の追加

- **基準**: 新しいメトリクスの追加時、メトリクス算出ロジックの実装のみで対応可能な設計とする。CPG抽出・CLI・設定管理等の既存コンポーネントへの変更を最小限に抑える
- **優先度**: Must
- **出典**: エージェント推測→ユーザー確認済み

### 4.5 ユーザビリティ

#### REQ-NF-007: ゼロコンフィグでの初回実行

- **基準**: 設定ファイルなしの状態で `kalos check .` を実行するだけで、デフォルトのルール・閾値で解析結果を得られる
- **優先度**: Must
- **出典**: エージェント推測→ユーザー確認済み

### 4.6 LLM連携時の可用性

#### REQ-NF-008: LLMフォールバック

- **基準**: LLM連携モード使用時にLLMが応答しない場合、テンプレートベースの改善提案にフォールバックする。kalos全体の動作がLLMの可用性に依存しない
- **優先度**: Must
- **出典**: ユーザー確認済み

### 4.7 外部通信とオフライン動作

#### REQ-NF-009: 外部通信の明示性と安全性

- **基準**:
  - ネットワーク通信は (a) CodeQL 管理対象 bundle の初回取得、(b) `--llm` 指定時の LLM 呼び出し、のみに限定する
  - CodeQL bundle 取得は固定バージョン + SHA-256 検証付きで行う
  - LLM の API キーは環境変数のみから取得し、設定ファイルへ保存しない
  - LLM outbound payload は allowlist 済み `LlmEnrichmentRequest` `{ rule_id, severity, language, repo_relative_path, metric?, pattern?, source_excerpt?, cpg_excerpt? }` のみを許可する
  - リポジトリ全体、診断対象外の周辺コード、環境変数、シークレット、絶対パスは LLM に送信しない
  - LLM 呼び出しは `connect timeout = 3s`, `overall timeout = 30s`, `retry = 0` とする
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
| 2 | WASM プラグイン SDK と manifest 配布形式 | REQ-FUNC-012, REQ-NF-006 | v1.1 設計 | SPI 契約自体は v1 で固定済み |
| 3 | 各言語の外部シンボル解決アダプタ実装 | REQ-FUNC-007 | 言語別設計ノート | 要件上の契約は固定済み |
| 4 | NF-001 の 60 秒目標と CodeQL 抽出時間の両立可能性 | REQ-NF-001 | ベンチマーク PoC | 未達なら代替アダプタを比較 |

## 要件間の関連

### 派生関係（derives from）

- REQ-FUNC-013 → REQ-FUNC-016: 閾値違反の診断（013）から重大度付与（016）が派生
- REQ-FUNC-008〜010 → REQ-FUNC-011: 各階層メトリクス（008〜010）から総合スコア（011）が派生

### 依存関係（depends on）

- REQ-FUNC-008〜010 は REQ-FUNC-001〜004（CPG生成）に依存
- REQ-FUNC-013 は REQ-FUNC-008〜010（メトリクス算出）に依存
- REQ-FUNC-015 は REQ-FUNC-013（診断報告）, REQ-FUNC-001〜004（該当コード断片取得）, REQ-NF-008〜010（LLM連携制約）に依存
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
| 0.2.0 | 2026-03-19 | メトリクス数式・総合スコア集約・重大度境界・差分解析契約・CodeQL 自動取得・LLM 入力制約を確定 | Codex |
| 0.1.0 | 2026-03-18 | 初版作成 | Claude（requirements-definer スキル） |
