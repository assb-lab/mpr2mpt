# mpr2mpt

BioLogic EC-Lab のバイナリ `.mpr` を、タブ区切りの測定表 `.mpt` に変換します。ビルドには Rust 1.91 以上が必要です。

```sh
cargo install --path . --locked
mpr2mpt sample.MPR
mpr2mpt sample.MPR -o result.mpt
```

既定では入力と同じフォルダに `sample.mpt` を作成します。出力ファイルは変換完了時に作成され、既存ファイルへの書き込みはエラーになります。

読み取りには、入力全体とデコード済みの測定表を保持するメモリが必要です。読み取りライブラリの入力サイズ上限は 2 GiB です。

出力は `EC-Lab ASCII FILE` で始まる3行ヘッダーと、MPR の格納順で並んだ全測定行・全列です。改行は CRLF、文字コードは ISO-8859-1、小数点は `.` です。経過秒と電流の符号は格納値のままです。EC-Lab の書き出し時に計算される量は、MPR に格納されている場合に含まれます。

## ターミナル画面

```sh
mpr2mpt-tui
mpr2mpt-tui sample.MPR -o result.mpt
```

入力欄にはパスを入力または貼り付けできます。`Tab` で欄を切り替え、`Enter` で変換し、`Esc` で終了します。出力欄を空にすると CLI と同じ保存先を使います。完了時は出力パス・行数・列数を、失敗時は理由を表示します。欄を修正してその場で再試行できます。

## 対応確認

公開 MPR/MPT 9組の計21,216行を変換しました。生成した MPT を Galvani と NumPy で読み直し、MPR の列順と全セルを元の格納型で照合しました。「版」は EC-Lab 本体の版番号ではなく、MPR 内の `VMP data` 版です。

| 測定手法 | データ版 | 測定数 | 行数 |
| --- | --- | ---: | ---: |
| CV / Advanced CV | 2、11 | 4 | 20,841 |
| OCV | 11 | 1 | 43 |
| GCPL | 11 | 1 | 132 |
| PEIS | 3、11 | 2 | 116 |
| GEIS | 11 | 1 | 84 |

高周波 EIS の高調波など、公式 MPT で表示値が置き換わる列でも、出力は MPR の格納値を使用します。非先頭の複合フラグ、複数のデータモジュール、未知の列や版はエラーになります。Windows 版 EC-Lab での直接読み込みと Igor CV Analyzer が要求する詳細ヘッダーは未検証です。 [yadg の公開データ](https://github.com/dgbowl/yadg/tree/21d443c05f0320834851f2d912fbfb911ef2d9cc/tests/test_x_eclab) [Galvani の公開 EIS データ](https://github.com/echemdata/galvani/tree/d4a5f444b16eae57d2edfad6df774ead95c92fd6/tests/testdata/v1150)

[変換仕様の調査記録](reports/BioLogic%20MPR%20MPT%20%E5%A4%89%E6%8F%9B%E4%BB%95%E6%A7%98.md)
