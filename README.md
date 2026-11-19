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
mpr2mpt-tui /path/to/measurements
mpr2mpt-tui sample.MPR -o result.mpt
```

親フォルダ・現在の一覧・プレビューを表示します。MPT は先頭 128 KiB から最大 24 行を表で表示します。MPS・TXT・Markdown・CSV・TSV は先頭 128 KiB をテキストで表示します。TXT などを一覧から選ぶには `m` で全項目表示に切り替えます。`l` または `→` でプレビューペインに移ります。端末が狭いときは一覧とプレビューを上下に並べます。

ファイルの種類をアイコンで表示します。フォルダは青、BioLogic ファイルは拡張子ごとの色になります。Nerd Font を使わない端末では `mpr2mpt-tui --plain-icons` で標準の文字記号に切り替えられます。

| キー | 操作 |
| --- | --- |
| `↑` / `↓` または `k` / `j` | 一覧の項目またはプレビュー内の行を移動 |
| `←` / `→` または `h` / `l` | 親フォルダ・一覧に戻る / フォルダ・プレビューに入る |
| `gg` / `G` | 一覧またはプレビュー範囲の先頭 / 末尾 |
| `gd` / `gh` / `gt` | `~/Downloads` / ホーム / ユーザーのゴミ箱 |
| `g` → `Space` | パスを入力して移動 |
| `f` | 空の入力欄を開き、入力に合わせてファイル名を絞り込む。`Enter` で確定、空欄で `Enter` を押すと解除。編集中は `Esc` で元の絞り込みに戻る |
| `m` | MPR・MPS・MPT とフォルダのみ / 全項目を切り替え |
| `.` | 隠しファイルと隠しフォルダの表示を切り替え。既定は非表示で、隠しファイルをパスで開くと表示がオンになる |
| `d` | カーソル位置の項目をゴミ箱へ移動。`y` または `Enter` で確定 |
| `Space` | MPR の選択を切り替え。移動後も選択を保持 |
| `x` | すべての選択を解除 |
| `yy` / `p` | 選択済みの MPR、またはカーソル位置の項目をコピー / 現在のフォルダに貼り付け |
| `cc` / `cd` / `cf` / `cn` | カーソル位置の絶対パス / 親フォルダのパス / ファイル名 / 拡張子を除いた名前をシステムクリップボードにコピー |
| `v` | MPR のメタデータと先頭最大100行をプレビューペインに表示。再度 `v` で閉じる。`l` / `→` で移ったあと、`j` / `k` で測定行、`J` / `K` でメタデータを移動 |
| `e` | 選択した MPR を一括変換。未選択ならカーソル位置の MPR を変換 |
| `Shift+E` | カーソル位置の MPR 1件について出力名またはパスを入力し、`Enter` で変換 |
| `Enter` | フォルダ・プレビューに入る、または結果から生成した MPT を開く |
| `Tab` | プレビューと直近の変換結果を切り替え。一覧を `j` / `k` で移動するとプレビューに戻る |
| `PageUp` / `PageDown`、`[` / `]` | プレビューの行・列を移動。結果画面では `PageUp` / `PageDown` でファイルを選ぶ |
| `o` | カーソル位置の項目を OS の既定アプリで開く。変換結果では選択中の出力を開く |
| `r` / `q` | 一覧を更新 / 終了 |
| `Shift+Q` | 現在のフォルダをシェルに返して終了 |

MPR のプレビューでは、格納された測定手法、測定開始日時、チャンネル、データ版のうち取得できる項目を表示します。変換時に保存先に既存ファイルが見つかると、対象を確認できます。`y` で上書き可能な出力を更新し、`s` で新規出力だけ作成します。`Esc` で変換を取り消します。

一括変換は Rayon で最大 2 ファイルずつ実行し、各 MPR と同じフォルダに `.mpt` を作成します。成功した選択は外れ、失敗した選択は再試行用に残ります。起動時の `-o` は単一入力用です。`Shift+E` の出力指定はその1件の変換に適用されます。相対パスは元の MPR のフォルダを基準とし、空欄のまま確定すると既定の `.mpt` 名を使います。

既定アプリで開くときは macOS の `open` または Linux の `xdg-open` を使います。

コピーと貼り付けは macOS と Linux に対応します。同名のファイルやフォルダが貼り付け先にある場合はその項目をスキップします。

パスのコピーは macOS の `pbcopy`、Linux の Wayland では `wl-copy`、X11 では `xclip` を使います。

ゴミ箱への移動は macOS の `/usr/bin/trash` または Linux の `/usr/bin/gio` を使います。`gt` は macOS の `~/.Trash` または Linux のユーザー用ゴミ箱を開きます。

### シェルでフォルダを移動して終了

`cargo install --path . --locked` でインストール後、zsh または Bash に次の関数を追加して `mcd` で起動します。`Shift+Q` でブラウズ中のフォルダへ移動します。直接 `mpr2mpt-tui` を起動した場合、`Shift+Q` は終了後にフォルダのパスを表示します。

```sh
mcd() {
  local scratch destination tui_exit
  scratch=$(mktemp -d) || return
  mpr2mpt-tui --cwd-file "$scratch/cwd" "$@"
  tui_exit=$?
  if (( tui_exit == 0 )) && [[ -f "$scratch/cwd" ]] &&
    IFS= read -r -d '' destination < "$scratch/cwd"; then
    cd -- "$destination" || tui_exit=$?
  fi
  rm -f -- "$scratch/cwd"
  rmdir -- "$scratch"
  return "$tui_exit"
}
```

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
