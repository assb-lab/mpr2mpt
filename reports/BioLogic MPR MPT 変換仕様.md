# BioLogic MPR → MPT の変換仕様

初版の `mpr2mpt` は、**完了済みの MPR に格納された測定表を MPT に変換する**。公開されている EC-Lab の出力例に沿い、3 行ヘッダー、タブ区切り、CRLF を既定とする案。出力対象は MPR に格納された行と列で、記録順、値、単位を維持する。まず CLI で解析ソフトへの読み込みを確かめ、その後で TUI から共通の変換処理を呼ぶ。 [BioLogic マニュアル §2.11.1、§5.2.1](https://mmrc.caltech.edu/BioLogic%20Echem/ECLab%20Manuals/EC-Lab%20software%20user%27s%20manual.pdf) [公開 MPR/MPT ペア](https://github.com/dgbowl/yadg/tree/61d55fe1fcf6cdd165b09a17ca99e261e1e34f50/tests/test_x_eclab)

## MPT の列は EC-Lab の書き出し条件で変わる

MPR は実験の測定値と設定を含むバイナリ。公式の **Export as Text** では標準の出力先は入力と同じディレクトリで、拡張子を `.mpt` に変える。時刻の表記、単位、桁数、書き出すサイクルやループを選べる。既定の相対時間でも、公開 CV の先頭の `time/s` は約 43,202 秒だった。CLI は MPR の時刻をそのまま転記する。 [BioLogic マニュアル §5.2.1](https://mmrc.caltech.edu/BioLogic%20Echem/ECLab%20Manuals/EC-Lab%20software%20user%27s%20manual.pdf) [CV 公開ペア](https://github.com/dgbowl/yadg/tree/61d55fe1fcf6cdd165b09a17ca99e261e1e34f50/tests/test_x_eclab)

同一測定の公開ペアでは、CV の `P/W` や、GCPL の `cycle number` が MPT 側にだけ現れる例を確認した。GCPL のその例では MPR のカウンターフラグは全行ゼロで、MPT はサイクル 0–3 を示す。EIS では高周波の一部の高調波列について、MPR の値と EC-Lab の書き出し値が異なるという検証記録もある。初版は格納列を基準にし、格納されていないサイクル番号や計算列の生成は後続の互換要件とする。平均電流の `<I>/mA` と瞬時電流の `I/mA` はそれぞれ元の列名と意味を保つ。 [公開ペア](https://github.com/dgbowl/yadg/tree/61d55fe1fcf6cdd165b09a17ca99e261e1e34f50/tests/test_x_eclab) [Galvani の v11.50 GCPL/EIS ペア](https://github.com/echemdata/galvani/tree/d4a5f444b16eae57d2edfad6df774ead95c92fd6/tests/testdata/v1150) [OpenReadout の検証記録](https://github.com/openreadout/openreadout/blob/main/docs/provenance/biologic-eclab.md) [公開クレートの列定義](https://docs.rs/crate/openreadout-echem/0.4.0/source/src/eclab.rs)

## CLI の既定は実例のある 3 行ヘッダー

```sh
mpr2mpt sample.MPR
mpr2mpt sample.MPR -o result.mpt
```

| 項目 | 初版の仕様案 |
|---|---|
| 入力 | 完了済みの BioLogic EC-Lab MPR 1 件。ファイル内容のシグネチャとデータ構造で検証する。 |
| 出力先 | 既定は入力と同じディレクトリに同じベース名の `.mpt`。明示先は `-o`。入力と出力先の既存ファイルを保護し、変換完了時だけ出力を確定する。 |
| ヘッダー | 1 行目は `EC-Lab ASCII FILE`、2 行目は `Nb header lines : 3`、3 行目はタブ区切りの列名。データは 4 行目から。 |
| 測定表 | 全記録行と格納列を元の順番で出力する。既知の複合フラグは対応する論理列に展開する。列名と単位は対応を検証した EC-Lab 表記を使う。 |
| テキスト表記 | タブ区切り、CRLF、BOM なし、小数点はピリオド。有限値は元の数値型に往復可能な精度で出力する。`µ` や `²` を含むラベルの文字コードは読み込み先の実測で確定する。 |
| 失敗 | 未対応の版や未知・曖昧な列、破損や切断、出力先の衝突、I/O エラーは理由を標準エラーに出し、非ゼロで終了する。診断は可能な範囲でファイル名、モジュール版、列 ID を示す。 |

3 行形式は Galvani の `bio_logic1.mpt` と `CV_C01.mpt` に実例があり、Galvani の読み込み実装は先頭行に CRLF を要求する。yadg は短いヘッダーを取得日時・設定情報なしの形式として扱う。Igor の既存 CV Analyzer を出力先に含める場合には別途対応が要る。同ツールは **4 行目の `Cyclic Voltammetry`** と、列名 `time/s`、`Ewe/V`、`<I>/mA`、`cycle number` を要求し、小数点にコンマを受け付けない。 [Galvani のサンプル](https://github.com/echemdata/galvani/tree/d4a5f444b16eae57d2edfad6df774ead95c92fd6/tests/testdata) [Galvani の MPT 読み込み](https://github.com/echemdata/galvani/blob/d4a5f444b16eae57d2edfad6df774ead95c92fd6/galvani/BioLogic.py) [yadg の MPT 読み込み](https://github.com/dgbowl/yadg/blob/main/src/yadg/extractors/eclab/mpt.py) [CV Analyzer のヘッダー検査](https://github.com/assb-lab/cv-analyzer/blob/7d49aa9a77fa9de0d9f7f168b383737dcf2b673c/CVAnalyzer.ipf#L126-L163) [CV Analyzer の数値検査](https://github.com/assb-lab/cv-analyzer/blob/7d49aa9a77fa9de0d9f7f168b383737dcf2b673c/CVAnalyzer.ipf#L514-L518)

## Rust リーダー候補を CV 4 対で照合した

`openreadout-echem` **0.4.0**（MIT OR Apache-2.0、Rust 1.91 以上）を一時プローブで実行した。通常 CV 3 対と Advanced CV 1 対は計 **20,841 行**で、EC-Lab が出力した同一測定の MPT と行数とサイクル番号が一致した。共通列の最大絶対差は `time/s` が **5.83×10⁻¹¹ s**、`Ewe/V` が **5.00×10⁻⁸ V**、`<I>/mA` が **7.11×10⁻¹⁵ mA** だった。今回の実行対象はリーダーであり、CLI による MPT 生成は実装段階の検証項目。 [公開クレート](https://docs.rs/crate/openreadout-echem/0.4.0) [照合した公開ペア](https://github.com/dgbowl/yadg/tree/61d55fe1fcf6cdd165b09a17ca99e261e1e34f50/tests/test_x_eclab)

EC-Lab 本体のバージョンと MPR の `VMP data` モジュール版は別の情報として扱う。公開 CV にはモジュール版 2 と 11 があり、版 11 の試料で 16 bit の列 ID 441 を確認した。リーダーは時刻列を先頭に移すため、出力では元の列 ID 順を復元する。また、リーダーはフラグの物理バイトをレコード先頭と見なす。フラグ列が先頭にない入力やデータモジュールが複数ある入力は、配置・連結の挙動を検証できた場合に受け付ける。 [公開クレートの MPR 実装](https://docs.rs/crate/openreadout-echem/0.4.0/source/src/eclab.rs) [Galvani のフラグと列 ID 処理](https://github.com/echemdata/galvani/blob/d4a5f444b16eae57d2edfad6df774ead95c92fd6/galvani/BioLogic.py)

## 対応範囲は実ファイルで確定する

CLI の受け入れ試験では、CV、OCV、GCPL、PEIS/GEIS の公開ペアから、実際に合格した手法とモジュール版の組み合わせを記録する。行数、格納列の順番と単位、フラグ、共通列の値を確認する。生成結果を独立した MPT リーダーで再読込し、格納精度での往復を検証する。公式 MPT との比較では表示丸めを考慮する。旧 CV の例では時刻が小数 2 桁で書き出され、格納値との差は最大 0.005 秒だった。途中で切れた入力と出力先衝突についても、出力確定前に正しく失敗することを確認する。Windows 版 EC-Lab に生成 MPT を直接読み込めるか、および新しい EC-Lab 版の全形式を読めるかは現時点で未検証。 [yadg の公開ペア](https://github.com/dgbowl/yadg/tree/61d55fe1fcf6cdd165b09a17ca99e261e1e34f50/tests/test_x_eclab) [Galvani の公開ペア](https://github.com/echemdata/galvani/tree/d4a5f444b16eae57d2edfad6df774ead95c92fd6/tests/testdata) [BioLogic の現行マニュアル一覧](https://my.biologic.net/documents/ec-lab-software-manuals/)

`Cargo.toml` には Clap と Ratatui があり、`src/main.rs` は雛形の状態。CLI と後続の TUI で変換関数とエラー型を共有し、TUI はファイルの選択、出力先、変換結果の表示を担当する。対応表の確定と同じ試験を共通処理に適用できる。 [Cargo.toml](../Cargo.toml) [src/main.rs](../src/main.rs)
