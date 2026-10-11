# UI/UX-Review – 11. Oktober 2026

Stand: `4e9f1d59`. Zielgruppe: Fotografen mit Erfahrung in Lightroom Classic und Capture One.

## Urteil

Die Grundarchitektur ist passend und die visuelle Richtung professionell: neutrale dunkle Flächen, das Foto als Hauptinhalt, Bibliothek/Entwickeln, kompakte Werkzeuggruppen, Histogramm und Filmstreifen. Die Oberfläche ist jedoch noch nicht durchgehend logisch und verlässlich genug, um von abgeschlossenem professionellem Feinschliff zu sprechen.

Capture-One-Anwender dürften sich bei Werkzeugleiste, Ebenen, Sessions und Exportrezepten schneller orientieren. Lightroom-Classic-Anwender erkennen Module, Bewertungsabläufe und Grundkorrekturen wieder, müssen aber insbesondere Werkzeugkürzel und die Verteilung der Werkzeuge umlernen. Das ist eine fachliche Einschätzung, kein Ergebnis eines Nutzertests.

## Prüfgrundlage und Grenzen

- Aktuellen React-/CSS-Code für Workspace, Bibliothek, Entwicklung, Ebenen, Export, Einstieg und Tastenkürzel gelesen.
- Vorhandene Screenshots von Bibliothek, Entwicklung und Export visuell angesehen. Diese sind nicht überall aktuell: Der jetzige Exportdialog hat bereits eine Rezept-Seitenleiste und eine feste Aktionsleiste.
- `StudioDevelop` und `StudioExport` aus dem aktuellen Quellcode isoliert im T3-Browser ausgeführt, mit synthetischen Store-Daten und simulierten Tauri-Aufrufen. Dabei wurden Maskenauswahl und Export-Einstellungen geprüft; keine Originalbilder oder nativen Einstellungen geändert.
- Die vollständige App läuft ohne Tauri-Brücke nicht im normalen Browser. Browser-Snapshots scheiterten technisch; DOM-Inspektion und gezielte Interaktionen funktionierten. Kein vollständiger nativer End-to-End-Test, keine Aussage über Renderqualität oder tatsächliche Exportdateien.
- Referenzkonventionen anhand offizieller Adobe- und Capture-One-Dokumentation abgeglichen.

## Befunde nach Priorität

### 1. Hoch: Tabwechsel ändert den Bearbeitungsbereich von lokal auf global

**Beleg:** `src/components/studio/StudioDevelop.tsx:586–599`, zusammen mit der Bestimmung von `layer` ab Zeile 494.

Beim Wechsel zu einem Tab außer „Ebenen“ werden `activeMaskContainerId` und `activeMaskId` gelöscht. Der Farb-Tab setzt außerdem `Panel.Adjustments`. Die Regler bearbeiten anschließend das ganze Foto.

**Reproduktion:** In der isolierten aktuellen Komponente eine aktive Ebene „Himmel“ gesetzt und auf „Color“ geklickt. Danach war `activeMaskContainerId === null` und `activePanel === 'adjustments'`.

**Auswirkung:** Wer die Farbe einer gewählten Maske ändern möchte, kann unbeabsichtigt eine globale Korrektur machen. Der Ebenenwähler im vollständigen Workspace bildet den Wechsel zwar ab, der Klick auf eine Werkzeugkategorie sollte diesen Wechsel aber nicht implizit auslösen.

**Empfehlung:** Werkzeugkategorie und Bearbeitungsziel getrennt verwalten. Die aktive Ebene bleibt beim Kategorienwechsel bestehen. Global-only-Werkzeuge benötigen einen klaren Hinweis oder einen ausdrücklichen Wechsel auf das gesamte Bild. Das aktuelle Ziel dauerhaft neben den Anpassungen anzeigen.

### 2. Hoch: Sichtbare Exportwerte können vom Exportauftrag abweichen

**Beleg:** `src/components/studio/StudioExport.tsx:157–162`, `loadRecipe`, `updateRecipe` und Rezeptliste ab Zeile 408.

Sobald ein Rezept angehakt ist, wird der Auftrag aus dessen gespeicherten Werten aufgebaut. Das weiterhin editierbare Formular rechts ist ein separater Entwurf. Änderungen dort gelten erst nach ausdrücklichem Aktualisieren des Rezepts.

**Reproduktion:** Gespeichertes und angehaktes Testrezept mit JPEG-Qualität 80. Das Formular zeigte zunächst 90 und ließ sich auf 100 ändern. Das gespeicherte Rezept und die für die Größenschätzung verwendeten Auftragseinstellungen blieben auf 80. Es wurde keine Datei exportiert.

Ein erklärender Hinweis und der Button „Export with Web 80“ sind vorhanden; das Verhalten ist also dokumentiert. Die Oberfläche verlangt trotzdem, dass der Anwender zwischen angehaktem Rezept, bearbeitetem Rezept und Entwurf unterscheidet, während andere Werte direkt vor ihm stehen.

**Empfehlung:** Den Modus deutlich benennen: „Aktuelle Einstellungen“ oder „Gespeicherte Rezepte“. Im Rezeptmodus rechts das ausgewählte Rezept samt Änderungsstatus zeigen. Ungespeicherte Änderungen müssen vor Export eindeutig berücksichtigt oder ausdrücklich verworfen werden. Eine kurze Ausgabezusammenfassung sollte die tatsächlich geplanten Werte zeigen.

### 3. Mittel: Der Standardtab priorisiert Spezialfunktionen vor Grundkorrekturen

**Beleg:** `src/components/studio/StudioDevelop.tsx:453`, `601`, `716–825`, `943`, `1621`.

„Anpassen“ ist intern `all` und enthält im geprüften JPEG-Zustand 18 Gruppen. Die geöffnete Gruppe „Ebenen & Masken“ mit zahlreichen Erstellungsaktionen kommt zuerst, dann Presets und Weißabgleich, danach Belichtung. Bei RAW kommen noch die Basischarakteristika dazu.

In der isolierten 300 px breiten, 800 px hohen Werkzeugspalte begann „Belichtung“ erst bei ungefähr y=658, „Präsenz“ bei y=906. Diese Messung betrifft die Testkomponente, nicht ein vollständiges natives Fenster; sie zeigt bereits die ungünstige Flächenverteilung.

**Empfehlung:** Einen echten Basis-/Schnelltab anbieten: Profil kompakt, Weißabgleich, Belichtung/Tonwerte, Präsenz. Den Ebenenstatus kompakt sichtbar halten, die große Maskenerstellung erst bei Bedarf öffnen. „Alle Werkzeuge“ kann eine zusätzliche Ansicht bleiben. Anpassen der Reihenfolge ist bereits möglich, ersetzt aber keinen guten Standard.

### 4. Mittel: Werkzeugkategorien haben unscharfe Grenzen

**Beleg:** `src/components/studio/StudioDevelop.tsx:1008`, `1043`, `1238`, `1531`, `1533`.

„Farbe“ enthält auch Klarheit, Struktur, Dunst entfernen, Tonkurve und Tonwerte. Objektivkorrektur liegt unter „Zuschnitt“. „Details“ enthält unter „Effekte“ auch Vignette, Filmkorn, Glow, Halation und LUT. Der Retusche-Tab zeigt Gesichtsretusche, während manuelle Retusche über das Werkzeug in der oberen Leiste erreichbar ist.

Keiner dieser Orte ist allein unvertretbar. Zusammen erschweren sie jedoch eine eindeutige Antwort auf „Wo gehört diese Funktion hin?“.

**Empfehlung:** Klare Aufgabenbereiche definieren. Beispielsweise Basis, Farbe, Details, Geometrie, Retusche; Ebenen als davon unabhängiges Bearbeitungsziel. Objektivkorrektur unter klar benannter Geometrie, kreative Looks bei Farbe/Looks und Retuschefunktionen gemeinsam auffindbar machen. Schnellzugriffe dürfen mehrere Zugänge bieten, sollten aber denselben Werkzeugzustand öffnen.

### 5. Mittel: Das Protokoll hilft zu wenig beim Wiederfinden einer Bearbeitung

**Beleg:** `src/components/studio/StudioHistory.tsx:14–30`, `src/components/studio/StudioDevelop.tsx:1621`.

Das Protokoll ist standardmäßig eingeklappt, steht am Ende von „Anpassen“ und ist in Spezialtabs nicht vorhanden. Einträge heißen nur „Anpassung 1“, „Anpassung 2“ usw.

**Auswirkung:** Der Anwender kann einen früheren Zustand anwählen, aber kaum erkennen, welcher Schritt gesucht ist.

**Empfehlung:** Von jedem Tab schnell erreichbar machen und Einträge mit Aktion, Wert und gegebenenfalls Ebene benennen, etwa „Belichtung +0,35 · Himmel“. Zusammenhängende Reglerbewegungen bleiben ein Schritt.

### 6. Mittel: Kleine Beschriftungen und schwache Aktionszustände bremsen den visuellen Feinschliff

**Beleg:** `src/components/studio/studio.css:338–346`, `386–419`, `472–505`.

Werkzeugtabs verwenden 9,5 px, Gruppenüberschriften und Regler überwiegend 11 px. Mehrere aktive Kopfaktionen werden im Ruhezustand auf 55 % Deckkraft reduziert. Das wirkt kompakt, macht aber insbesondere die Navigation klein und lässt verfügbare Aktionen weniger deutlich erscheinen.

Die neutralen Flächen, die gemeinsame Icon-Familie und tabellarischen Zahlen passen sehr gut zur Bildbearbeitung. Es geht hier um Lesbarkeit und eine präzisere Hierarchie innerhalb dieser Richtung.

**Empfehlung:** Hauptnavigation und wichtige Labels eher bei 11–12 px halten, sekundäre Informationen gezielt zurücknehmen und aktive, inaktive sowie deaktivierte Zustände klar unterscheiden. Hitboxen um kleine Icons ausreichend groß lassen. Eine optionale größere UI-Dichte bzw. UI-Skalierung wäre für lange Arbeitssitzungen sinnvoll. Desktopanforderungen gelten; pauschale mobile 44-px-Regeln sind hier nicht das passende Layoutziel.

### 7. Mittel: Der Einstieg für Umsteiger ist unnötig versteckt

**Beleg:** `src/components/session/SessionWorkspace.tsx:615–623`, `711–732`.

Der Willkommensbildschirm bietet Erstellen und Öffnen. Der Import aus Lightroom/Capture One liegt im Menü des Sessionnamens und ist nur bei bereits geöffnetem Katalog aktiviert.

**Auswirkung:** Ausgerechnet ein neuer Anwender mit bestehender Lightroom-Bibliothek muss zuerst das Rawtonic-Modell verstehen und einen Katalog erstellen, bevor er den passenden Einstieg nutzen kann.

**Empfehlung:** „Aus Lightroom / Capture One übernehmen“ als direkten Einstieg anbieten. Dieser Ablauf führt bei Bedarf selbst durch die Katalogerstellung. „Neue Session“, „Neuer Katalog“ und „Vorhandene Bibliothek öffnen“ sprachlich klar unterscheiden.

### 8. Mittel: Vertraute Kürzel stammen aus unterschiedlichen Bedienmodellen

**Beleg:** `src/utils/keyboardUtils.ts`, insbesondere die Werkzeugdefinitionen ab Zeile 299.

G/D, Bewertungen und P/X/U erleichtern den Einstieg. Bei Werkzeugen verwendet Rawtonic bewusst Capture-One-Kürzel: C für Zuschnitt, R für Ausrichten, B für Pinsel; K ist zusätzlich als Pinsel-Alias vorhanden. In Lightroom Classic aktiviert R dagegen den Zuschnitt. Die bestehende Capture-One-Vorgabe ist im Code ausdrücklich als frühere Nutzerentscheidung dokumentiert.

**Empfehlung:** Diese Vorgabe beibehalten, aber optionale Kürzelprofile „Rawtonic / Lightroom Classic / Capture One“ anbieten, ergänzt um eine kurze Übersicht. Einzeln anpassbare Kürzel sind bereits vorhanden. Auch die bestehende Links-/Rechts-Anordnung der Werkzeuge ließe sich über benannte Workspace-Vorgaben verständlicher entdecken.

## Was bereits gut sitzt

- Bibliothek/Entwickeln ist eine verständliche Hauptgliederung.
- Filmstreifen, Histogramm, Bewertungs- und Filterkonzepte entsprechen fotografischen Arbeitsabläufen.
- Die kompakte, neutrale Oberfläche gibt dem Bild Vorrang.
- Links oder rechts platzierbare Werkzeuge, veränderbare Panelbreiten und speicherbare Werkzeugreihenfolgen bieten sinnvolle Anpassbarkeit.
- Die Regler unterstützen Schieben, Zahlenfelder, Tastatur und Ziehen am Label.
- Der aktuelle Exportdialog hat bereits die sinnvolle räumliche Trennung von Rezepten und Einstellungen sowie eine feste Abschlussaktion.
- Fokusmarkierungen, benannte Icon-Buttons und Dialog-Fokusverwaltung sind im Code vorhanden; eine vollständige Accessibility-Zertifizierung wurde nicht durchgeführt.

## Empfohlene Reihenfolge

1. Bearbeitungsziel bei Tabwechsel und Exportzustände eindeutig machen.
2. Standardtab und Werkzeugkategorien nach den häufigsten Aufgaben ordnen.
3. Protokoll und Umsteiger-Einstieg verbessern.
4. Typografie, Kontraste und Workspace-/Kürzelprofile verfeinern.

Danach mit echten Lightroom- und Capture-One-Anwendern fünf Aufgaben beobachten: bestehenden Bestand öffnen/importieren, Favoriten auswählen, ein Bild grundentwickeln, eine Maske farblich korrigieren und zwei Ausgabevarianten exportieren. Suchwege, Fehlbedienungen und notwendige Hilfestellungen erfassen. Erst das bestätigt, wie gut Umsteiger tatsächlich zurechtkommen.

## Referenzen

- [Adobe: Workspace basics](https://helpx.adobe.com/lightroom-classic/desktop/workspace/workspace-basics.html)
- [Adobe: Keyboard shortcuts](https://helpx.adobe.com/fi/lightroom-classic/desktop/introduction-to-lightroom-classic/keyboard-shortcuts.html)
- [Capture One: User interface overview](https://support.captureone.com/hc/en-us/articles/360002468797-User-interface-overview)
- [Capture One: Tool Tabs – Library](https://support.captureone.com/hc/en-us/articles/360002477117-Tool-Tabs-Library)

## Bewertung und Umsetzung (11. Oktober 2026, Branch `claude/ui-review-fixes`)

Gegen den Code geprüft. Maßstab ist Rawtonics Regel: pro Werkzeug die bessere Lösung von Lightroom und Capture One, ein Werkzeug, keine Editor-Modi.

| Befund | Bewertung | Umgesetzt |
| --- | --- | --- |
| 1. Tabwechsel setzt Ebene zurück | Zutreffend. Die Werkzeuge in Farbe/Details können bereits auf Ebenen wirken, nur `selectTab` hat das Ziel gelöscht. | Die gewählte Ebene bleibt in jedem Tab außer Zuschnitt das Ziel, wie beim Ebenenwähler von Capture One. In Tabs ohne Ebenenliste zeigt eine Zeile „Bearbeitet Ebene …“ mit „Hintergrund bearbeiten“. Im Retusche-Tab erklärt ein Hinweis, dass die Gesichtsretusche auf das ganze Foto wirkt. |
| 2. Exportwerte ≠ Exportauftrag | Zutreffend. | Wie bei Capture One werden Änderungen am gezeigten Rezept direkt darin gespeichert (der Knopf „… aktualisieren“ entfällt). Die Liste beginnt mit „Aktuelle Einstellungen“, die beim Wechsel zu einem Rezept erhalten bleiben. Eine Statuszeile sagt, was rechts bearbeitet wird und ob es exportiert wird. „Dieser Export“ listet pro Auftrag Format, Qualität, Größe und Ziel, für ein gerade bearbeitetes Rezept schon vor dem Speichern. |
| 3. Standardtab priorisiert Spezialfunktionen | Teilweise. Ebenen oben im Anpassen-Tab entspricht Capture One, aber das leere Raster mit 11 Kacheln verdrängt Belichtung. | Ohne Ebenen zeigt der Anpassen-Tab nur noch eine Zeile; das „+“ im Kopf erstellt Ebenen, das Raster bleibt im Ebenen-Tab. Presets waren schon eingeklappt. Einen zusätzlichen Basis-Tab gibt es nicht: Er wäre ein zweiter Zugang zu denselben Werkzeugen. |
| 4. Unscharfe Kategorien | Teilweise berechtigt. Die Neuordnung der Tabs ist eine Produktentscheidung, die einen Seitenvergleich verdient. | Nicht umgesetzt. In PARITY_GAPS G67 festgehalten. |
| 5. Protokoll | Zutreffend. | Einträge heißen jetzt nach ihrer Änderung („Belichtung +0,35“, „Belichtung, Kontrast +1“, „Belichtung +0,35 · Himmel“, „Neue Ebene Himmel“). Das Protokoll steht in jedem Tab. |
| 6. Kleine Beschriftungen und schwache Zustände | Teilweise. | Tabbeschriftungen 9,5 → 10,5 px. Kopfaktionen im Ruhezustand 55 → 75 % Deckkraft, deaktivierte Aktionen 35 % und ohne Hover-Aufhellung. Protokollzeilen 10 → 11 px. Eine UI-Skalierung ist offen (G54). |
| 7. Einstieg für Umsteiger | Zutreffend. | Der Willkommensbildschirm bietet „Aus Lightroom oder Capture One übernehmen“: zuerst ein Katalog ohne Session/Katalog-Auswahl, dann sofort der Import. Der Menüeintrag führt ohne offenen Katalog durch denselben Ablauf, statt deaktiviert zu sein. |
| 8. Kürzelprofile LR/C1 | Abgelehnt. Wählbare Profile „Lightroom Classic / Capture One“ wären genau die Editor-Modi, die die Regel ausschließt (G53, Entscheidung 2026-10-08). | Nichts. Einzeln anpassbare Kürzel bleiben der Weg. |

Nicht geprüft: das Verhalten in der laufenden nativen App, Messungen im echten Fenster und der empfohlene Test mit Lightroom- und Capture-One-Anwendern. Abgesichert ist es durch Vitest-Tests für Ebenenziel, kompakten Anpassen-Tab, Protokollnamen, automatisches Speichern von Rezepten samt Exportübersicht und den Einstieg über den Willkommensbildschirm.

## Folgearbeit (11. Oktober 2026, Branch `claude/ui-tabs-scale`)

**Werkzeugkategorien (Befund 4).** Jedes Werkzeug steht jetzt in genau einem Tab (Test `shows every tool in one tab only`); nur das Protokoll steht in allen Tabs.

| Tab | Werkzeuge |
| --- | --- |
| Anpassen | Ebenen (kompakt), Stile & Presets, Basischarakteristik (Profil, Kurve), Weißabgleich, Belichtung, Präsenz, Tonkurve, Tonwerte, Effekte |
| Farbe | Farbmischer, Farb-Editor, Color Grading, Schwarzweiß, Kalibrierung |
| Details | Schärfen, Rauschreduzierung, Farbsaum, Moiré |
| Zuschnitt, Geometrie & Objektiv | Zuschnitt, Geometrie, Objektivkorrektur |
| Retusche | Gesichtsretusche |
| Ebenen | Ebenenliste und die Werkzeuge, die auf Ebenen wirken |

Vorher zeigte „Anpassen“ alle 18 Gruppen, und die übrigen Tabs filterten daraus. Ein eigener Ton-Tab war geplant, wurde aber nicht gebaut. Grundlage dafür war ein Abzählen von Tabwechseln anhand des dokumentierten Aufbaus von Capture One 16 und Lightroom Classic (notes `docs/reference/ui-c1-lr.md`), kein Nutzertest. Capture One führt Tonkurve, Tonwerte, Klarheit und Vignettierung zusammen mit Belichtung im Tab Adjustments. Lightroom stellt Basic und Tone Curve direkt untereinander. Ein Ton-Tab hätte für „grundentwickeln, dann Kurve“ einen Wechsel mehr gekostet als bei beiden Vorbildern. Die Effekte stehen nicht im Retusche-Tab, weil die Gesichtsretusche beim Anzeigen Gesichter sucht; Capture One führt die Vignettierung ebenfalls unter Adjustments.

**UI-Größe (Befund 6).** Einstellungen › Allgemein › Oberflächengröße: 100 / 112 / 125 %. Vergrößert werden die Werkzeugspalten sowie die Leisten über und unter Bild und Raster, per CSS `zoom`. Der Bildbereich selbst wird nicht skaliert; die native Darstellung darunter behält ihre Lage. Die obere Leiste bleibt unverändert, weil macOS die Fensterknöpfe darin platziert. Menüs und Popovers bleiben bei 100 %.

**In der laufenden App geprüft** (QA-Build, eigene Identität, Session-Kopie von drei CC0-RAWs):
- Ebene im Anpassen-Tab angelegt, zu Farbe gewechselt: Die Ebene bleibt das Ziel, die Zeile „Bearbeitet: …“ erscheint. Im Retusche-Tab erscheint der Hinweis. Zuschnitt setzt das Ziel auf das Foto zurück.
- Protokoll: „Neue Ebene Layer 1“, „Crop“, „Exposure +0.35“.
- Export: Rezept gespeichert, Qualität auf 77 geändert. Das Rezept wurde automatisch gespeichert, und die exportierte JPEG hat die Qualität 77 (aus der Quantisierungstabelle bestimmt).
- 125 %: Die Rechtecke der Elemente stimmen mit der Bildschirmposition überein. Ein nativer Drag am Kontrast-Regler: Der Griff folgt dem Zeiger. Das +-Menü öffnet am Knopf, und das Bild sitzt im verkleinerten Bildbereich.
- Willkommensbildschirm mit drei Einstiegen. Der Menüeintrag „Import …“ öffnet in einer Session das Katalogformular.

Dabei gefunden und behoben:
- „Bearbeitet Ebene Ebene 1“ heißt jetzt „Bearbeitet: Ebene 1“.
- Ein gezeigtes, nicht angehaktes Rezept erschien im Plan als „Aktuelle Einstellungen“.
- Der Plan lag außerhalb des sichtbaren Bereichs; er steht jetzt in der Fußleiste.
- Das Importformular sprach von „Session“ und „Auftrag“.

Nicht durchgeführt: das Anlegen eines Katalogs über den nativen Ordnerdialog (durch Vitest abgedeckt). Der Nutzertest entfällt (Entscheidung 2026-10-11).
