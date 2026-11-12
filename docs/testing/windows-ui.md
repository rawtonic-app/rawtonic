# Windows UI regression checks

Run on Windows 11 at 200% display DPI, with a German keyboard layout, against
an isolated QA library and copies of RAW files. Repeat the visual checks at
100%, 112% and 125% interface size. Record the build commit and screenshots; a unit
test pass does not establish native window behavior or text geometry.

| Case | Steps | Required result |
| --- | --- | --- |
| Shortcut tooltips | Open Develop and inspect Undo, Redo, Export, Preferences, Copy/Paste, Sync, Auto WB and Auto adjustment. | Ctrl/Shift/Alt labels, no Command/Shift/Option glyphs. |
| Preferences key | From welcome and from a library, physically press Ctrl+comma on the German layout. | Preferences opens; Escape closes it. Also repeat with the English layout. |
| Photo count | Open a folder with eight RAW photos, then export into Output and return to Capture. Navigate to another folder. | Header photo count follows the loaded grid, including after navigation; Output exports do not inflate Capture. |
| Filename and dimensions | Open a photo with a long filename, resize the window gradually at both interface scales. | Filename shortens with ellipsis; dimensions remain whole when their group has enough width. Other controls do not overlap. |
| Close feedback | Hold the mouse over Close, then hold the left button down; cancel by moving away before releasing. | Pressed red is darker than hover; maximize keeps its own pressed feedback. |
| Window floor | Resize by dragging and by Win32 MoveWindow to 300×200 and 1200×700 physical pixels at 200% DPI. | Interactive minimum follows interface size, capped to the work area minus the frame: 1000×600, 1120×672, 1250×750 logical respectively. Win32 MoveWindow retains the 1000×600 emergency floor. Check that Close is wholly visible and the header name/count remain readable; no navigator-only strip. Maximize, restore, snap and move between monitors of different DPI remain usable. |
| Launch file | Start the app with an ARW path, first with no remembered library, then with a remembered library. Repeat via Open With in an existing app. | Test a photo inside and outside the remembered library: both appear in Develop after restoration, the inside photo is selected, the outside photo is not imported. Also test no remembered library and a failed restore. Welcome does not cover the photo; decode succeeds. Preview JPEGs without EXIF do not produce a missing-EXIF ERROR. |

On macOS, smoke-check the native title bar, original shortcut notation, window
restoration and the Develop footer at both interface scales. The new chrome
CSS and minimum-size enforcement are scoped away from macOS.
