/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

// German translations for Qwen Code CLI
// Deutsche Ubersetzungen fur Qwen Code CLI

export default {
  // ============================================================================
  // Help / UI Components
  // ============================================================================
  'Basics:': 'Grundlagen:',
  'Add context': 'Kontext hinzufugen',
  'Use {{symbol}} to specify files for context (e.g., {{example}}) to target specific files or folders.':
    'Verwenden Sie {{symbol}}, um Dateien als Kontext anzugeben (z.B. {{example}}), um bestimmte Dateien oder Ordner auszuwahlen.',
  '@': '@',
  '@src/myFile.ts': '@src/myFile.ts',
  'Shell mode': 'Shell-Modus',
  'YOLO mode': 'YOLO-Modus',
  'plan mode': 'Planungsmodus',
  'auto-accept edits': 'Anderungen automatisch akzeptieren',
  'Accepting edits': 'Anderungen werden akzeptiert',
  '(shift + tab to cycle)': '(Umschalt + Tab zum Wechseln)',
  'Execute shell commands via {{symbol}} (e.g., {{example1}}) or use natural language (e.g., {{example2}}).':
    'Shell-Befehle uber {{symbol}} ausfuhren (z.B. {{example1}}) oder naturliche Sprache verwenden (z.B. {{example2}}).',
  '!': '!',
  '!npm run start': '!npm run start',
  'start server': 'Server starten',
  'Commands:': 'Befehle:',
  'shell command': 'Shell-Befehl',
  'Model Context Protocol command (from external servers)':
    'Model Context Protocol Befehl (von externen Servern)',
  'Keyboard Shortcuts:': 'Tastenkurzel:',
  'Jump through words in the input': 'Worter in der Eingabe uberspringen',
  'Close dialogs, cancel requests, or quit application':
    'Dialoge schliessen, Anfragen abbrechen oder Anwendung beenden',
  'New line': 'Neue Zeile',
  'New line (Alt+Enter works for certain linux distros)':
    'Neue Zeile (Alt+Enter funktioniert bei bestimmten Linux-Distributionen)',
  'Clear the screen': 'Bildschirm loschen',
  'Open input in external editor': 'Eingabe in externem Editor offnen',
  'Send message': 'Nachricht senden',
  'Initializing...': 'Initialisierung...',
  'Connecting to MCP servers... ({{connected}}/{{total}})':
    'Verbindung zu MCP-Servern wird hergestellt... ({{connected}}/{{total}})',
  'Type your message or @path/to/file': 'Nachricht eingeben oder @Pfad/zur/Datei',
  "Press 'i' for INSERT mode and 'Esc' for NORMAL mode.":
    "Drucken Sie 'i' fur den EINFUGE-Modus und 'Esc' fur den NORMAL-Modus.",
  'Cancel operation / Clear input (double press)':
    'Vorgang abbrechen / Eingabe loschen (doppelt drucken)',
  'Cycle approval modes': 'Genehmigungsmodi durchschalten',
  'Cycle through your prompt history': 'Eingabeverlauf durchblattern',
  'For a full list of shortcuts, see {{docPath}}':
    'Eine vollstandige Liste der Tastenkurzel finden Sie unter {{docPath}}',
  'docs/keyboard-shortcuts.md': 'docs/keyboard-shortcuts.md',
  'for help on Qwen Code': 'fur Hilfe zu Qwen Code',
  'show version info': 'Versionsinformationen anzeigen',
  'submit a bug report': 'Fehlerbericht einreichen',
  'About Qwen Code': 'Uber Qwen Code',

  // ============================================================================
  // System Information Fields
  // ============================================================================
  'CLI Version': 'CLI-Version',
  'Git Commit': 'Git-Commit',
  Model: 'Modell',
  Sandbox: 'Sandbox',
  'OS Platform': 'Betriebssystem',
  'OS Arch': 'OS-Architektur',
  'OS Release': 'OS-Version',
  'Node.js Version': 'Node.js-Version',
  'NPM Version': 'NPM-Version',
  'Session ID': 'Sitzungs-ID',
  'Auth Method': 'Authentifizierungsmethode',
  'Base URL': 'Basis-URL',
  'Memory Usage': 'Speichernutzung',
  'IDE Client': 'IDE-Client',

  // ============================================================================
  // Commands - General
  // ============================================================================
  'Analyzes the project and creates a tailored QWEN.md file.':
    'Analysiert das Projekt und erstellt eine massgeschneiderte QWEN.md-Datei.',
  'list available Qwen Code tools. Usage: /tools [desc]':
    'Verfugbare Qwen Code Werkzeuge auflisten. Verwendung: /tools [desc]',
  'Available Qwen Code CLI tools:': 'Verfugbare Qwen Code CLI-Werkzeuge:',
  'No tools available': 'Keine Werkzeuge verfugbar',
  'View or change the approval mode for tool usage':
    'Genehmigungsmodus fur Werkzeugnutzung anzeigen oder andern',
  'View or change the language setting': 'Spracheinstellung anzeigen oder andern',
  'change the theme': 'Design andern',
  'Select Theme': 'Design auswahlen',
  Preview: 'Vorschau',
  '(Use Enter to select, Tab to configure scope)':
    '(Enter zum Auswahlen, Tab zum Konfigurieren des Bereichs)',
  '(Use Enter to apply scope, Tab to select theme)':
    '(Enter zum Anwenden des Bereichs, Tab zum Auswahlen des Designs)',
  'Theme configuration unavailable due to NO_COLOR env variable.':
    'Design-Konfiguration aufgrund der NO_COLOR-Umgebungsvariable nicht verfugbar.',
  'Theme "{{themeName}}" not found.': 'Design "{{themeName}}" nicht gefunden.',
  'Theme "{{themeName}}" not found in selected scope.':
    'Design "{{themeName}}" im ausgewahlten Bereich nicht gefunden.',
  'Clear conversation history and free up context':
    'Gesprachsverlauf loschen und Kontext freigeben',
  'Compresses the context by replacing it with a summary.':
    'Komprimiert den Kontext durch Ersetzen mit einer Zusammenfassung.',
  'open full Qwen Code documentation in your browser':
    'Vollstandige Qwen Code Dokumentation im Browser offnen',
  'Configuration not available.': 'Konfiguration nicht verfugbar.',
  'change the auth method': 'Authentifizierungsmethode andern',
  'Copy the last result or code snippet to clipboard':
    'Letztes Ergebnis oder Codeausschnitt in die Zwischenablage kopieren',

  // ============================================================================
  // Commands - Agents
  // ============================================================================
  'Manage subagents for specialized task delegation.':
    'Unteragenten fur spezialisierte Aufgabendelegation verwalten.',
  'Manage existing subagents (view, edit, delete).':
    'Bestehende Unteragenten verwalten (anzeigen, bearbeiten, loschen).',
  'Create a new subagent with guided setup.':
    'Neuen Unteragenten mit gefuhrter Einrichtung erstellen.',

  // ============================================================================
  // Agents - Management Dialog
  // ============================================================================
  Agents: 'Agenten',
  'Choose Action': 'Aktion wahlen',
  'Edit {{name}}': '{{name}} bearbeiten',
  'Edit Tools: {{name}}': 'Werkzeuge bearbeiten: {{name}}',
  'Edit Color: {{name}}': 'Farbe bearbeiten: {{name}}',
  'Delete {{name}}': '{{name}} loschen',
  'Unknown Step': 'Unbekannter Schritt',
  'Esc to close': 'Esc zum Schliessen',
  'Enter to select, ↑↓ to navigate, Esc to close':
    'Enter zum Auswahlen, ↑↓ zum Navigieren, Esc zum Schliessen',
  'Esc to go back': 'Esc zum Zuruckgehen',
  'Enter to confirm, Esc to cancel': 'Enter zum Bestatigen, Esc zum Abbrechen',
  'Enter to select, ↑↓ to navigate, Esc to go back':
    'Enter zum Auswahlen, ↑↓ zum Navigieren, Esc zum Zuruckgehen',
  'Invalid step: {{step}}': 'Ungultiger Schritt: {{step}}',
  'No subagents found.': 'Keine Unteragenten gefunden.',
  "Use '/agents create' to create your first subagent.":
    "Verwenden Sie '/agents create', um Ihren ersten Unteragenten zu erstellen.",
  '(built-in)': '(integriert)',
  '(overridden by project level agent)': '(uberschrieben durch Projektagent)',
  'Project Level ({{path}})': 'Projektebene ({{path}})',
  'User Level ({{path}})': 'Benutzerebene ({{path}})',
  'Built-in Agents': 'Integrierte Agenten',
  'Using: {{count}} agents': 'Verwendet: {{count}} Agenten',
  'View Agent': 'Agent anzeigen',
  'Edit Agent': 'Agent bearbeiten',
  'Delete Agent': 'Agent loschen',
  Back: 'Zuruck',
  'No agent selected': 'Kein Agent ausgewahlt',
  'File Path: ': 'Dateipfad: ',
  'Tools: ': 'Werkzeuge: ',
  'Color: ': 'Farbe: ',
  'Description:': 'Beschreibung:',
  'System Prompt:': 'System-Prompt:',
  'Open in editor': 'Im Editor offnen',
  'Edit tools': 'Werkzeuge bearbeiten',
  'Edit color': 'Farbe bearbeiten',
  '❌ Error:': '❌ Fehler:',
  'Are you sure you want to delete agent "{{name}}"?':
    'Sind Sie sicher, dass Sie den Agenten "{{name}}" loschen mochten?',
  // ============================================================================
  // Agents - Creation Wizard
  // ============================================================================
  'Project Level (.qwen/agents/)': 'Projektebene (.qwen/agents/)',
  'User Level (~/.qwen/agents/)': 'Benutzerebene (~/.qwen/agents/)',
  '✅ Subagent Created Successfully!': '✅ Unteragent erfolgreich erstellt!',
  'Subagent "{{name}}" has been saved to {{level}} level.':
    'Unteragent "{{name}}" wurde auf {{level}}-Ebene gespeichert.',
  'Name: ': 'Name: ',
  'Location: ': 'Speicherort: ',
  '❌ Error saving subagent:': '❌ Fehler beim Speichern des Unteragenten:',
  'Warnings:': 'Warnungen:',
  'Name "{{name}}" already exists at {{level}} level - will overwrite existing subagent':
    'Name "{{name}}" existiert bereits auf {{level}}-Ebene - bestehender Unteragent wird uberschrieben',
  'Name "{{name}}" exists at user level - project level will take precedence':
    'Name "{{name}}" existiert auf Benutzerebene - Projektebene hat Vorrang',
  'Name "{{name}}" exists at project level - existing subagent will take precedence':
    'Name "{{name}}" existiert auf Projektebene - bestehender Unteragent hat Vorrang',
  'Description is over {{length}} characters':
    'Beschreibung ist uber {{length}} Zeichen',
  'System prompt is over {{length}} characters':
    'System-Prompt ist uber {{length}} Zeichen',
  // Agents - Creation Wizard Steps
  'Step {{n}}: Choose Location': 'Schritt {{n}}: Speicherort wahlen',
  'Step {{n}}: Choose Generation Method':
    'Schritt {{n}}: Generierungsmethode wahlen',
  'Generate with Qwen Code (Recommended)':
    'Mit Qwen Code generieren (Empfohlen)',
  'Manual Creation': 'Manuelle Erstellung',
  'Describe what this subagent should do and when it should be used. (Be comprehensive for best results)':
    'Beschreiben Sie, was dieser Unteragent tun soll und wann er verwendet werden soll. (Ausfuhrliche Beschreibung fur beste Ergebnisse)',
  'e.g., Expert code reviewer that reviews code based on best practices...':
    'z.B. Experte fur Code-Reviews, der Code nach Best Practices uberpruft...',
  'Generating subagent configuration...':
    'Unteragent-Konfiguration wird generiert...',
  'Failed to generate subagent: {{error}}':
    'Fehler beim Generieren des Unteragenten: {{error}}',
  'Step {{n}}: Describe Your Subagent': 'Schritt {{n}}: Unteragent beschreiben',
  'Step {{n}}: Enter Subagent Name': 'Schritt {{n}}: Unteragent-Name eingeben',
  'Step {{n}}: Enter System Prompt': 'Schritt {{n}}: System-Prompt eingeben',
  'Step {{n}}: Enter Description': 'Schritt {{n}}: Beschreibung eingeben',
  // Agents - Tool Selection
  'Step {{n}}: Select Tools': 'Schritt {{n}}: Werkzeuge auswahlen',
  'All Tools (Default)': 'Alle Werkzeuge (Standard)',
  'All Tools': 'Alle Werkzeuge',
  'Read-only Tools': 'Nur-Lese-Werkzeuge',
  'Read & Edit Tools': 'Lese- und Bearbeitungswerkzeuge',
  'Read & Edit & Execution Tools': 'Lese-, Bearbeitungs- und Ausfuhrungswerkzeuge',
  'All tools selected, including MCP tools':
    'Alle Werkzeuge ausgewahlt, einschliesslich MCP-Werkzeuge',
  'Selected tools:': 'Ausgewahlte Werkzeuge:',
  'Read-only tools:': 'Nur-Lese-Werkzeuge:',
  'Edit tools:': 'Bearbeitungswerkzeuge:',
  'Execution tools:': 'Ausfuhrungswerkzeuge:',
  'Step {{n}}: Choose Background Color': 'Schritt {{n}}: Hintergrundfarbe wahlen',
  'Step {{n}}: Confirm and Save': 'Schritt {{n}}: Bestatigen und Speichern',
  // Agents - Navigation & Instructions
  'Esc to cancel': 'Esc zum Abbrechen',
  'Press Enter to save, e to save and edit, Esc to go back':
    'Enter zum Speichern, e zum Speichern und Bearbeiten, Esc zum Zuruckgehen',
  'Press Enter to continue, {{navigation}}Esc to {{action}}':
    'Enter zum Fortfahren, {{navigation}}Esc zum {{action}}',
  cancel: 'Abbrechen',
  'go back': 'Zuruckgehen',
  '↑↓ to navigate, ': '↑↓ zum Navigieren, ',
  'Enter a clear, unique name for this subagent.':
    'Geben Sie einen eindeutigen Namen fur diesen Unteragenten ein.',
  'e.g., Code Reviewer': 'z.B. Code-Reviewer',
  'Name cannot be empty.': 'Name darf nicht leer sein.',
  "Write the system prompt that defines this subagent's behavior. Be comprehensive for best results.":
    'Schreiben Sie den System-Prompt, der das Verhalten dieses Unteragenten definiert. Ausfuhrlich fur beste Ergebnisse.',
  'e.g., You are an expert code reviewer...':
    'z.B. Sie sind ein Experte fur Code-Reviews...',
  'System prompt cannot be empty.': 'System-Prompt darf nicht leer sein.',
  'Describe when and how this subagent should be used.':
    'Beschreiben Sie, wann und wie dieser Unteragent verwendet werden soll.',
  'e.g., Reviews code for best practices and potential bugs.':
    'z.B. Uberpruft Code auf Best Practices und mogliche Fehler.',
  'Description cannot be empty.': 'Beschreibung darf nicht leer sein.',
  'Failed to launch editor: {{error}}': 'Fehler beim Starten des Editors: {{error}}',
  'Failed to save and edit subagent: {{error}}':
    'Fehler beim Speichern und Bearbeiten des Unteragenten: {{error}}',

  // ============================================================================
  // Commands - General (continued)
  // ============================================================================
  'View and edit Qwen Code settings': 'Qwen Code Einstellungen anzeigen und bearbeiten',
  Settings: 'Einstellungen',
  '(Use Enter to select{{tabText}})': '(Enter zum Auswahlen{{tabText}})',
  ', Tab to change focus': ', Tab zum Fokuswechsel',
  'To see changes, Qwen Code must be restarted. Press r to exit and apply changes now.':
    'Um Anderungen zu sehen, muss Qwen Code neu gestartet werden. Drucken Sie r, um jetzt zu beenden und Anderungen anzuwenden.',
  'The command "/{{command}}" is not supported in non-interactive mode.':
    'Der Befehl "/{{command}}" wird im nicht-interaktiven Modus nicht unterstutzt.',
  // ============================================================================
  // Settings Labels
  // ============================================================================
  'Vim Mode': 'Vim-Modus',
  'Disable Auto Update': 'Automatische Updates deaktivieren',
  'Enable Prompt Completion': 'Eingabevervollstandigung aktivieren',
  'Debug Keystroke Logging': 'Debug-Protokollierung von Tastatureingaben',
  Language: 'Sprache',
  'Output Format': 'Ausgabeformat',
  'Hide Window Title': 'Fenstertitel ausblenden',
  'Show Status in Title': 'Status im Titel anzeigen',
  'Hide Tips': 'Tipps ausblenden',
  'Hide Banner': 'Banner ausblenden',
  'Hide Context Summary': 'Kontextzusammenfassung ausblenden',
  'Hide CWD': 'Arbeitsverzeichnis ausblenden',
  'Hide Sandbox Status': 'Sandbox-Status ausblenden',
  'Hide Model Info': 'Modellinformationen ausblenden',
  'Hide Footer': 'Fusszeile ausblenden',
  'Show Memory Usage': 'Speichernutzung anzeigen',
  'Show Line Numbers': 'Zeilennummern anzeigen',
  'Show Citations': 'Quellenangaben anzeigen',
  'Custom Witty Phrases': 'Benutzerdefinierte Witzige Spruche',
  'Enable Welcome Back': 'Willkommen-zuruck aktivieren',
  'Disable Loading Phrases': 'Ladespruche deaktivieren',
  'Screen Reader Mode': 'Bildschirmleser-Modus',
  'IDE Mode': 'IDE-Modus',
  'Max Session Turns': 'Maximale Sitzungsrunden',
  'Skip Next Speaker Check': 'Nachste-Sprecher-Prufung uberspringen',
  'Skip Loop Detection': 'Schleifenerkennung uberspringen',
  'Skip Startup Context': 'Startkontext uberspringen',
  'Enable OpenAI Logging': 'OpenAI-Protokollierung aktivieren',
  'OpenAI Logging Directory': 'OpenAI-Protokollierungsverzeichnis',
  Timeout: 'Zeitlimit',
  'Max Retries': 'Maximale Wiederholungen',
  'Disable Cache Control': 'Cache-Steuerung deaktivieren',
  'Memory Discovery Max Dirs': 'Maximale Verzeichnisse fur Speichererkennung',
  'Load Memory From Include Directories':
    'Speicher aus Include-Verzeichnissen laden',
  'Respect .gitignore': '.gitignore beachten',
  'Respect .qwenignore': '.qwenignore beachten',
  'Enable Recursive File Search': 'Rekursive Dateisuche aktivieren',
  'Disable Fuzzy Search': 'Unscharfe Suche deaktivieren',
  'Enable Interactive Shell': 'Interaktive Shell aktivieren',
  'Show Color': 'Farbe anzeigen',
  'Auto Accept': 'Automatisch akzeptieren',
  'Use Ripgrep': 'Ripgrep verwenden',
  'Use Builtin Ripgrep': 'Integriertes Ripgrep verwenden',
  'Enable Tool Output Truncation': 'Werkzeugausgabe-Kurzung aktivieren',
  'Tool Output Truncation Threshold': 'Schwellenwert fur Werkzeugausgabe-Kurzung',
  'Tool Output Truncation Lines': 'Zeilen fur Werkzeugausgabe-Kurzung',
  'Folder Trust': 'Ordnervertrauen',
  'Vision Model Preview': 'Vision-Modell-Vorschau',
  'Tool Schema Compliance': 'Werkzeug-Schema-Konformitat',
  // Settings enum options
  'Auto (detect from system)': 'Automatisch (vom System erkennen)',
  Text: 'Text',
  JSON: 'JSON',
  Plan: 'Plan',
  Default: 'Standard',
  'Auto Edit': 'Automatisch bearbeiten',
  YOLO: 'YOLO',
  'toggle vim mode on/off': 'Vim-Modus ein-/ausschalten',
  'check session stats. Usage: /stats [model|tools]':
    'Sitzungsstatistiken prufen. Verwendung: /stats [model|tools]',
  'Show model-specific usage statistics.':
    'Modellspezifische Nutzungsstatistiken anzeigen.',
  'Show tool-specific usage statistics.':
    'Werkzeugspezifische Nutzungsstatistiken anzeigen.',
  'exit the cli': 'CLI beenden',
  'list configured MCP servers and tools, or authenticate with OAuth-enabled servers':
    'Konfigurierte MCP-Server und Werkzeuge auflisten oder mit OAuth-fahigen Servern authentifizieren',
  'Manage workspace directories': 'Arbeitsbereichsverzeichnisse verwalten',
  'Add directories to the workspace. Use comma to separate multiple paths':
    'Verzeichnisse zum Arbeitsbereich hinzufugen. Komma zum Trennen mehrerer Pfade verwenden',
  'Show all directories in the workspace':
    'Alle Verzeichnisse im Arbeitsbereich anzeigen',
  'set external editor preference': 'Externen Editor festlegen',
  'Manage extensions': 'Erweiterungen verwalten',
  'List active extensions': 'Aktive Erweiterungen auflisten',
  'Update extensions. Usage: update <extension-names>|--all':
    'Erweiterungen aktualisieren. Verwendung: update <Erweiterungsnamen>|--all',
  'manage IDE integration': 'IDE-Integration verwalten',
  'check status of IDE integration': 'Status der IDE-Integration prufen',
  'install required IDE companion for {{ideName}}':
    'Erforderlichen IDE-Begleiter fur {{ideName}} installieren',
  'enable IDE integration': 'IDE-Integration aktivieren',
  'disable IDE integration': 'IDE-Integration deaktivieren',
  'IDE integration is not supported in your current environment. To use this feature, run Qwen Code in one of these supported IDEs: VS Code or VS Code forks.':
    'IDE-Integration wird in Ihrer aktuellen Umgebung nicht unterstutzt. Um diese Funktion zu nutzen, fuhren Sie Qwen Code in einer dieser unterstutzten IDEs aus: VS Code oder VS Code-Forks.',
  'Set up GitHub Actions': 'GitHub Actions einrichten',
  'Configure terminal keybindings for multiline input (VS Code, Cursor, Windsurf, Trae)':
    'Terminal-Tastenbelegungen fur mehrzeilige Eingabe konfigurieren (VS Code, Cursor, Windsurf, Trae)',
  'Please restart your terminal for the changes to take effect.':
    'Bitte starten Sie Ihr Terminal neu, damit die Anderungen wirksam werden.',
  'Failed to configure terminal: {{error}}':
    'Fehler beim Konfigurieren des Terminals: {{error}}',
  'Could not determine {{terminalName}} config path on Windows: APPDATA environment variable is not set.':
    'Konnte {{terminalName}}-Konfigurationspfad unter Windows nicht ermitteln: APPDATA-Umgebungsvariable ist nicht gesetzt.',
  '{{terminalName}} keybindings.json exists but is not a valid JSON array. Please fix the file manually or delete it to allow automatic configuration.':
    '{{terminalName}} keybindings.json existiert, ist aber kein gultiges JSON-Array. Bitte korrigieren Sie die Datei manuell oder loschen Sie sie, um automatische Konfiguration zu ermoglichen.',
  'File: {{file}}': 'Datei: {{file}}',
  'Failed to parse {{terminalName}} keybindings.json. The file contains invalid JSON. Please fix the file manually or delete it to allow automatic configuration.':
    'Fehler beim Parsen von {{terminalName}} keybindings.json. Die Datei enthalt ungultiges JSON. Bitte korrigieren Sie die Datei manuell oder loschen Sie sie, um automatische Konfiguration zu ermoglichen.',
  'Error: {{error}}': 'Fehler: {{error}}',
  'Shift+Enter binding already exists': 'Umschalt+Enter-Belegung existiert bereits',
  'Ctrl+Enter binding already exists': 'Strg+Enter-Belegung existiert bereits',
  'Existing keybindings detected. Will not modify to avoid conflicts.':
    'Bestehende Tastenbelegungen erkannt. Keine Anderungen, um Konflikte zu vermeiden.',
  'Please check and modify manually if needed: {{file}}':
    'Bitte prufen und bei Bedarf manuell andern: {{file}}',
  'Added Shift+Enter and Ctrl+Enter keybindings to {{terminalName}}.':
    'Umschalt+Enter und Strg+Enter Tastenbelegungen zu {{terminalName}} hinzugefugt.',
  'Modified: {{file}}': 'Geandert: {{file}}',
  '{{terminalName}} keybindings already configured.':
    '{{terminalName}}-Tastenbelegungen bereits konfiguriert.',
  'Failed to configure {{terminalName}}.':
    'Fehler beim Konfigurieren von {{terminalName}}.',
  'Your terminal is already configured for an optimal experience with multiline input (Shift+Enter and Ctrl+Enter).':
    'Ihr Terminal ist bereits fur optimale Erfahrung mit mehrzeiliger Eingabe konfiguriert (Umschalt+Enter und Strg+Enter).',
  'Could not detect terminal type. Supported terminals: VS Code, Cursor, Windsurf, and Trae.':
    'Terminal-Typ konnte nicht erkannt werden. Unterstutzte Terminals: VS Code, Cursor, Windsurf und Trae.',
  'Terminal "{{terminal}}" is not supported yet.':
    'Terminal "{{terminal}}" wird noch nicht unterstutzt.',

  // ============================================================================
  // Commands - Language
  // ============================================================================
  'Invalid language. Available: en-US, zh-CN':
    'Ungultige Sprache. Verfugbar: en-US, zh-CN',
  'Language subcommands do not accept additional arguments.':
    'Sprach-Unterbefehle akzeptieren keine zusatzlichen Argumente.',
  'Current UI language: {{lang}}': 'Aktuelle UI-Sprache: {{lang}}',
  'Current LLM output language: {{lang}}':
    'Aktuelle LLM-Ausgabesprache: {{lang}}',
  'LLM output language not set': 'LLM-Ausgabesprache nicht festgelegt',
  'Set UI language': 'UI-Sprache festlegen',
  'Set LLM output language': 'LLM-Ausgabesprache festlegen',
  'Usage: /language ui [zh-CN|en-US]': 'Verwendung: /language ui [zh-CN|en-US]',
  'Usage: /language output <language>': 'Verwendung: /language output <Sprache>',
  'Example: /language output 中文': 'Beispiel: /language output Deutsch',
  'Example: /language output English': 'Beispiel: /language output English',
  'Example: /language output 日本語': 'Beispiel: /language output Japanisch',
  'UI language changed to {{lang}}': 'UI-Sprache geandert zu {{lang}}',
  'LLM output language rule file generated at {{path}}':
    'LLM-Ausgabesprach-Regeldatei generiert unter {{path}}',
  'Please restart the application for the changes to take effect.':
    'Bitte starten Sie die Anwendung neu, damit die Anderungen wirksam werden.',
  'Failed to generate LLM output language rule file: {{error}}':
    'Fehler beim Generieren der LLM-Ausgabesprach-Regeldatei: {{error}}',
  'Invalid command. Available subcommands:':
    'Ungultiger Befehl. Verfugbare Unterbefehle:',
  'Available subcommands:': 'Verfugbare Unterbefehle:',
  'To request additional UI language packs, please open an issue on GitHub.':
    'Um zusatzliche UI-Sprachpakete anzufordern, offnen Sie bitte ein Issue auf GitHub.',
  'Available options:': 'Verfugbare Optionen:',
  '  - zh-CN: Simplified Chinese': '  - zh-CN: Vereinfachtes Chinesisch',
  '  - en-US: English': '  - en-US: Englisch',
  'Set UI language to Simplified Chinese (zh-CN)':
    'UI-Sprache auf Vereinfachtes Chinesisch (zh-CN) setzen',
  'Set UI language to English (en-US)': 'UI-Sprache auf Englisch (en-US) setzen',

  // ============================================================================
  // Commands - Approval Mode
  // ============================================================================
  'Approval Mode': 'Genehmigungsmodus',
  'Current approval mode: {{mode}}': 'Aktueller Genehmigungsmodus: {{mode}}',
  'Available approval modes:': 'Verfugbare Genehmigungsmodi:',
  'Approval mode changed to: {{mode}}': 'Genehmigungsmodus geandert zu: {{mode}}',
  'Approval mode changed to: {{mode}} (saved to {{scope}} settings{{location}})':
    'Genehmigungsmodus geandert zu: {{mode}} (gespeichert in {{scope}} Einstellungen{{location}})',
  'Usage: /approval-mode <mode> [--session|--user|--project]':
    'Verwendung: /approval-mode <Modus> [--session|--user|--project]',

  'Scope subcommands do not accept additional arguments.':
    'Bereichs-Unterbefehle akzeptieren keine zusatzlichen Argumente.',
  'Plan mode - Analyze only, do not modify files or execute commands':
    'Planungsmodus - Nur analysieren, keine Dateien andern oder Befehle ausfuhren',
  'Default mode - Require approval for file edits or shell commands':
    'Standardmodus - Genehmigung fur Dateibearbeitungen oder Shell-Befehle erforderlich',
  'Auto-edit mode - Automatically approve file edits':
    'Automatischer Bearbeitungsmodus - Dateibearbeitungen automatisch genehmigen',
  'YOLO mode - Automatically approve all tools':
    'YOLO-Modus - Alle Werkzeuge automatisch genehmigen',
  '{{mode}} mode': '{{mode}}-Modus',
  'Settings service is not available; unable to persist the approval mode.':
    'Einstellungsdienst nicht verfugbar; Genehmigungsmodus kann nicht gespeichert werden.',
  'Failed to save approval mode: {{error}}':
    'Fehler beim Speichern des Genehmigungsmodus: {{error}}',
  'Failed to change approval mode: {{error}}':
    'Fehler beim Andern des Genehmigungsmodus: {{error}}',
  'Apply to current session only (temporary)':
    'Nur auf aktuelle Sitzung anwenden (temporar)',
  'Persist for this project/workspace': 'Fur dieses Projekt/Arbeitsbereich speichern',
  'Persist for this user on this machine':
    'Fur diesen Benutzer auf diesem Computer speichern',
  'Analyze only, do not modify files or execute commands':
    'Nur analysieren, keine Dateien andern oder Befehle ausfuhren',
  'Require approval for file edits or shell commands':
    'Genehmigung fur Dateibearbeitungen oder Shell-Befehle erforderlich',
  'Automatically approve file edits': 'Dateibearbeitungen automatisch genehmigen',
  'Automatically approve all tools': 'Alle Werkzeuge automatisch genehmigen',
  'Workspace approval mode exists and takes priority. User-level change will have no effect.':
    'Arbeitsbereich-Genehmigungsmodus existiert und hat Vorrang. Benutzerebene-Anderung hat keine Wirkung.',
  '(Use Enter to select, Tab to change focus)':
    '(Enter zum Auswahlen, Tab zum Fokuswechsel)',
  'Apply To': 'Anwenden auf',
  'User Settings': 'Benutzereinstellungen',
  'Workspace Settings': 'Arbeitsbereich-Einstellungen',

  // ============================================================================
  // Commands - Memory
  // ============================================================================
  'Commands for interacting with memory.':
    'Befehle fur die Interaktion mit dem Speicher.',
  'Show the current memory contents.': 'Aktuellen Speicherinhalt anzeigen.',
  'Show project-level memory contents.': 'Projektebene-Speicherinhalt anzeigen.',
  'Show global memory contents.': 'Globalen Speicherinhalt anzeigen.',
  'Add content to project-level memory.':
    'Inhalt zum Projektebene-Speicher hinzufugen.',
  'Add content to global memory.': 'Inhalt zum globalen Speicher hinzufugen.',
  'Refresh the memory from the source.': 'Speicher aus der Quelle aktualisieren.',
  'Usage: /memory add --project <text to remember>':
    'Verwendung: /memory add --project <zu merkender Text>',
  'Usage: /memory add --global <text to remember>':
    'Verwendung: /memory add --global <zu merkender Text>',
  'Attempting to save to project memory: "{{text}}"':
    'Versuche im Projektspeicher zu speichern: "{{text}}"',
  'Attempting to save to global memory: "{{text}}"':
    'Versuche im globalen Speicher zu speichern: "{{text}}"',
  'Current memory content from {{count}} file(s):':
    'Aktueller Speicherinhalt aus {{count}} Datei(en):',
  'Memory is currently empty.': 'Speicher ist derzeit leer.',
  'Project memory file not found or is currently empty.':
    'Projektspeicherdatei nicht gefunden oder derzeit leer.',
  'Global memory file not found or is currently empty.':
    'Globale Speicherdatei nicht gefunden oder derzeit leer.',
  'Global memory is currently empty.': 'Globaler Speicher ist derzeit leer.',
  'Global memory content:\n\n---\n{{content}}\n---':
    'Globaler Speicherinhalt:\n\n---\n{{content}}\n---',
  'Project memory content from {{path}}:\n\n---\n{{content}}\n---':
    'Projektspeicherinhalt von {{path}}:\n\n---\n{{content}}\n---',
  'Project memory is currently empty.': 'Projektspeicher ist derzeit leer.',
  'Refreshing memory from source files...':
    'Speicher wird aus Quelldateien aktualisiert...',
  'Add content to the memory. Use --global for global memory or --project for project memory.':
    'Inhalt zum Speicher hinzufugen. --global fur globalen Speicher oder --project fur Projektspeicher verwenden.',
  'Usage: /memory add [--global|--project] <text to remember>':
    'Verwendung: /memory add [--global|--project] <zu merkender Text>',
  'Attempting to save to memory {{scope}}: "{{fact}}"':
    'Versuche im Speicher {{scope}} zu speichern: "{{fact}}"',

  // ============================================================================
  // Commands - MCP
  // ============================================================================
  'Authenticate with an OAuth-enabled MCP server':
    'Mit einem OAuth-fahigen MCP-Server authentifizieren',
  'List configured MCP servers and tools':
    'Konfigurierte MCP-Server und Werkzeuge auflisten',
  'Restarts MCP servers.': 'MCP-Server neu starten.',
  'Config not loaded.': 'Konfiguration nicht geladen.',
  'Could not retrieve tool registry.': 'Werkzeugregister konnte nicht abgerufen werden.',
  'No MCP servers configured with OAuth authentication.':
    'Keine MCP-Server mit OAuth-Authentifizierung konfiguriert.',
  'MCP servers with OAuth authentication:':
    'MCP-Server mit OAuth-Authentifizierung:',
  'Use /mcp auth <server-name> to authenticate.':
    'Verwenden Sie /mcp auth <Servername> zur Authentifizierung.',
  "MCP server '{{name}}' not found.": "MCP-Server '{{name}}' nicht gefunden.",
  "Successfully authenticated and refreshed tools for '{{name}}'.":
    "Erfolgreich authentifiziert und Werkzeuge fur '{{name}}' aktualisiert.",
  "Failed to authenticate with MCP server '{{name}}': {{error}}":
    "Authentifizierung mit MCP-Server '{{name}}' fehlgeschlagen: {{error}}",
  "Re-discovering tools from '{{name}}'...":
    "Werkzeuge von '{{name}}' werden neu erkannt...",

  // ============================================================================
  // Commands - Chat
  // ============================================================================
  'Manage conversation history.': 'Gesprachsverlauf verwalten.',
  'List saved conversation checkpoints': 'Gespeicherte Gesprachspruefpunkte auflisten',
  'No saved conversation checkpoints found.':
    'Keine gespeicherten Gesprachsprufpunkte gefunden.',
  'List of saved conversations:': 'Liste gespeicherter Gesprache:',
  'Note: Newest last, oldest first': 'Hinweis: Neueste zuletzt, alteste zuerst',
  'Save the current conversation as a checkpoint. Usage: /chat save <tag>':
    'Aktuelles Gesprach als Prufpunkt speichern. Verwendung: /chat save <Tag>',
  'Missing tag. Usage: /chat save <tag>':
    'Tag fehlt. Verwendung: /chat save <Tag>',
  'Delete a conversation checkpoint. Usage: /chat delete <tag>':
    'Gesprachsprufpunkt loschen. Verwendung: /chat delete <Tag>',
  'Missing tag. Usage: /chat delete <tag>':
    'Tag fehlt. Verwendung: /chat delete <Tag>',
  "Conversation checkpoint '{{tag}}' has been deleted.":
    "Gesprachsprufpunkt '{{tag}}' wurde geloscht.",
  "Error: No checkpoint found with tag '{{tag}}'.":
    "Fehler: Kein Prufpunkt mit Tag '{{tag}}' gefunden.",
  'Resume a conversation from a checkpoint. Usage: /chat resume <tag>':
    'Gesprach von einem Prufpunkt fortsetzen. Verwendung: /chat resume <Tag>',
  'Missing tag. Usage: /chat resume <tag>':
    'Tag fehlt. Verwendung: /chat resume <Tag>',
  'No saved checkpoint found with tag: {{tag}}.':
    'Kein gespeicherter Prufpunkt mit Tag gefunden: {{tag}}.',
  'A checkpoint with the tag {{tag}} already exists. Do you want to overwrite it?':
    'Ein Prufpunkt mit dem Tag {{tag}} existiert bereits. Mochten Sie ihn uberschreiben?',
  'No chat client available to save conversation.':
    'Kein Chat-Client verfugbar, um Gesprach zu speichern.',
  'Conversation checkpoint saved with tag: {{tag}}.':
    'Gesprachsprufpunkt gespeichert mit Tag: {{tag}}.',
  'No conversation found to save.': 'Kein Gesprach zum Speichern gefunden.',
  'No chat client available to share conversation.':
    'Kein Chat-Client verfugbar, um Gesprach zu teilen.',
  'Invalid file format. Only .md and .json are supported.':
    'Ungultiges Dateiformat. Nur .md und .json werden unterstutzt.',
  'Error sharing conversation: {{error}}':
    'Fehler beim Teilen des Gesprachs: {{error}}',
  'Conversation shared to {{filePath}}': 'Gesprach geteilt nach {{filePath}}',
  'No conversation found to share.': 'Kein Gesprach zum Teilen gefunden.',
  'Share the current conversation to a markdown or json file. Usage: /chat share <file>':
    'Aktuelles Gesprach in eine Markdown- oder JSON-Datei teilen. Verwendung: /chat share <Datei>',

  // ============================================================================
  // Commands - Summary
  // ============================================================================
  'Generate a project summary and save it to .qwen/PROJECT_SUMMARY.md':
    'Projektzusammenfassung generieren und in .qwen/PROJECT_SUMMARY.md speichern',
  'No chat client available to generate summary.':
    'Kein Chat-Client verfugbar, um Zusammenfassung zu generieren.',
  'Already generating summary, wait for previous request to complete':
    'Zusammenfassung wird bereits generiert, warten Sie auf Abschluss der vorherigen Anfrage',
  'No conversation found to summarize.': 'Kein Gesprach zum Zusammenfassen gefunden.',
  'Failed to generate project context summary: {{error}}':
    'Fehler beim Generieren der Projektkontextzusammenfassung: {{error}}',
  'Saved project summary to {{filePathForDisplay}}.':
    'Projektzusammenfassung gespeichert unter {{filePathForDisplay}}.',
  'Saving project summary...': 'Projektzusammenfassung wird gespeichert...',
  'Generating project summary...': 'Projektzusammenfassung wird generiert...',
  'Failed to generate summary - no text content received from LLM response':
    'Fehler beim Generieren der Zusammenfassung - kein Textinhalt von LLM-Antwort erhalten',

  // ============================================================================
  // Commands - Model
  // ============================================================================
  'Switch the model for this session': 'Modell fur diese Sitzung wechseln',
  'Content generator configuration not available.':
    'Inhaltsgenerator-Konfiguration nicht verfugbar.',
  'Authentication type not available.': 'Authentifizierungstyp nicht verfugbar.',
  'No models available for the current authentication type ({{authType}}).':
    'Keine Modelle fur den aktuellen Authentifizierungstyp ({{authType}}) verfugbar.',

  // ============================================================================
  // Commands - Clear
  // ============================================================================
  'Starting a new session, resetting chat, and clearing terminal.':
    'Neue Sitzung wird gestartet, Chat wird zuruckgesetzt und Terminal wird geloscht.',
  'Starting a new session and clearing.':
    'Neue Sitzung wird gestartet und geloscht.',

  // ============================================================================
  // Commands - Compress
  // ============================================================================
  'Already compressing, wait for previous request to complete':
    'Komprimierung lauft bereits, warten Sie auf Abschluss der vorherigen Anfrage',
  'Failed to compress chat history.': 'Fehler beim Komprimieren des Chatverlaufs.',
  'Failed to compress chat history: {{error}}':
    'Fehler beim Komprimieren des Chatverlaufs: {{error}}',
  'Compressing chat history': 'Chatverlauf wird komprimiert',
  'Chat history compressed from {{originalTokens}} to {{newTokens}} tokens.':
    'Chatverlauf komprimiert von {{originalTokens}} auf {{newTokens}} Token.',
  'Compression was not beneficial for this history size.':
    'Komprimierung war fur diese Verlaufsgross nicht vorteilhaft.',
  'Chat history compression did not reduce size. This may indicate issues with the compression prompt.':
    'Chatverlauf-Komprimierung hat die Grosse nicht reduziert. Dies kann auf Probleme mit dem Komprimierungs-Prompt hindeuten.',
  'Could not compress chat history due to a token counting error.':
    'Chatverlauf konnte aufgrund eines Token-Zahlfehlers nicht komprimiert werden.',
  'Chat history is already compressed.': 'Chatverlauf ist bereits komprimiert.',

  // ============================================================================
  // Commands - Directory
  // ============================================================================
  'Configuration is not available.': 'Konfiguration ist nicht verfugbar.',
  'Please provide at least one path to add.':
    'Bitte geben Sie mindestens einen Pfad zum Hinzufugen an.',
  'The /directory add command is not supported in restrictive sandbox profiles. Please use --include-directories when starting the session instead.':
    'Der Befehl /directory add wird in restriktiven Sandbox-Profilen nicht unterstutzt. Bitte verwenden Sie --include-directories beim Starten der Sitzung.',
  "Error adding '{{path}}': {{error}}": "Fehler beim Hinzufugen von '{{path}}': {{error}}",
  'Successfully added QWEN.md files from the following directories if there are:\n- {{directories}}':
    'QWEN.md-Dateien aus folgenden Verzeichnissen erfolgreich hinzugefugt, falls vorhanden:\n- {{directories}}',
  'Error refreshing memory: {{error}}': 'Fehler beim Aktualisieren des Speichers: {{error}}',
  'Successfully added directories:\n- {{directories}}':
    'Verzeichnisse erfolgreich hinzugefugt:\n- {{directories}}',
  'Current workspace directories:\n{{directories}}':
    'Aktuelle Arbeitsbereichsverzeichnisse:\n{{directories}}',

  // ============================================================================
  // Commands - Docs
  // ============================================================================
  'Please open the following URL in your browser to view the documentation:\n{{url}}':
    'Bitte offnen Sie folgende URL in Ihrem Browser, um die Dokumentation anzusehen:\n{{url}}',
  'Opening documentation in your browser: {{url}}':
    'Dokumentation wird in Ihrem Browser geoffnet: {{url}}',

  // ============================================================================
  // Dialogs - Tool Confirmation
  // ============================================================================
  'Do you want to proceed?': 'Mochten Sie fortfahren?',
  'Yes, allow once': 'Ja, einmal erlauben',
  'Allow always': 'Immer erlauben',
  No: 'Nein',
  'No (esc)': 'Nein (Esc)',
  'Yes, allow always for this session': 'Ja, fur diese Sitzung immer erlauben',
  'Modify in progress:': 'Anderung in Bearbeitung:',
  'Save and close external editor to continue':
    'Speichern und externen Editor schliessen, um fortzufahren',
  'Apply this change?': 'Diese Anderung anwenden?',
  'Yes, allow always': 'Ja, immer erlauben',
  'Modify with external editor': 'Mit externem Editor bearbeiten',
  'No, suggest changes (esc)': 'Nein, Anderungen vorschlagen (Esc)',
  "Allow execution of: '{{command}}'?": "Ausfuhrung erlauben von: '{{command}}'?",
  'Yes, allow always ...': 'Ja, immer erlauben ...',
  'Yes, and auto-accept edits': 'Ja, und Anderungen automatisch akzeptieren',
  'Yes, and manually approve edits': 'Ja, und Anderungen manuell genehmigen',
  'No, keep planning (esc)': 'Nein, weiter planen (Esc)',
  'URLs to fetch:': 'Abzurufende URLs:',
  'MCP Server: {{server}}': 'MCP-Server: {{server}}',
  'Tool: {{tool}}': 'Werkzeug: {{tool}}',
  'Allow execution of MCP tool "{{tool}}" from server "{{server}}"?':
    'Ausfuhrung des MCP-Werkzeugs "{{tool}}" von Server "{{server}}" erlauben?',
  'Yes, always allow tool "{{tool}}" from server "{{server}}"':
    'Ja, Werkzeug "{{tool}}" von Server "{{server}}" immer erlauben',
  'Yes, always allow all tools from server "{{server}}"':
    'Ja, alle Werkzeuge von Server "{{server}}" immer erlauben',

  // ============================================================================
  // Dialogs - Shell Confirmation
  // ============================================================================
  'Shell Command Execution': 'Shell-Befehlsausfuhrung',
  'A custom command wants to run the following shell commands:':
    'Ein benutzerdefinierter Befehl mochte folgende Shell-Befehle ausfuhren:',

  // ============================================================================
  // Dialogs - Pro Quota
  // ============================================================================
  'Pro quota limit reached for {{model}}.':
    'Pro-Kontingentlimit fur {{model}} erreicht.',
  'Change auth (executes the /auth command)':
    'Authentifizierung andern (fuhrt den /auth-Befehl aus)',
  'Continue with {{model}}': 'Mit {{model}} fortfahren',

  // ============================================================================
  // Dialogs - Welcome Back
  // ============================================================================
  'Current Plan:': 'Aktueller Plan:',
  'Progress: {{done}}/{{total}} tasks completed':
    'Fortschritt: {{done}}/{{total}} Aufgaben abgeschlossen',
  ', {{inProgress}} in progress': ', {{inProgress}} in Bearbeitung',
  'Pending Tasks:': 'Ausstehende Aufgaben:',
  'What would you like to do?': 'Was mochten Sie tun?',
  'Choose how to proceed with your session:':
    'Wahlen Sie, wie Sie mit Ihrer Sitzung fortfahren mochten:',
  'Start new chat session': 'Neue Chat-Sitzung starten',
  'Continue previous conversation': 'Vorheriges Gesprach fortsetzen',
  '👋 Welcome back! (Last updated: {{timeAgo}})':
    '👋 Willkommen zuruck! (Zuletzt aktualisiert: {{timeAgo}})',
  '🎯 Overall Goal:': '🎯 Gesamtziel:',

  // ============================================================================
  // Dialogs - Auth
  // ============================================================================
  'Get started': 'Loslegen',
  'How would you like to authenticate for this project?':
    'Wie mochten Sie sich fur dieses Projekt authentifizieren?',
  'OpenAI API key is required to use OpenAI authentication.':
    'OpenAI API-Schlussel ist fur die OpenAI-Authentifizierung erforderlich.',
  'You must select an auth method to proceed. Press Ctrl+C again to exit.':
    'Sie mussen eine Authentifizierungsmethode wahlen, um fortzufahren. Drucken Sie erneut Strg+C zum Beenden.',
  '(Use Enter to Set Auth)': '(Enter zum Festlegen der Authentifizierung)',
  'Terms of Services and Privacy Notice for Qwen Code':
    'Nutzungsbedingungen und Datenschutzhinweis fur Qwen Code',
  'Qwen OAuth': 'Qwen OAuth',
  OpenAI: 'OpenAI',
  'Failed to login. Message: {{message}}':
    'Anmeldung fehlgeschlagen. Meldung: {{message}}',
  'Authentication is enforced to be {{enforcedType}}, but you are currently using {{currentType}}.':
    'Authentifizierung ist auf {{enforcedType}} festgelegt, aber Sie verwenden derzeit {{currentType}}.',
  'Qwen OAuth authentication timed out. Please try again.':
    'Qwen OAuth-Authentifizierung abgelaufen. Bitte versuchen Sie es erneut.',
  'Qwen OAuth authentication cancelled.':
    'Qwen OAuth-Authentifizierung abgebrochen.',
  'Qwen OAuth Authentication': 'Qwen OAuth-Authentifizierung',
  'Please visit this URL to authorize:': 'Bitte besuchen Sie diese URL zur Autorisierung:',
  'Or scan the QR code below:': 'Oder scannen Sie den QR-Code unten:',
  'Waiting for authorization': 'Warten auf Autorisierung',
  'Time remaining:': 'Verbleibende Zeit:',
  '(Press ESC or CTRL+C to cancel)': '(ESC oder STRG+C zum Abbrechen drucken)',
  'Qwen OAuth Authentication Timeout': 'Qwen OAuth-Authentifizierung abgelaufen',
  'OAuth token expired (over {{seconds}} seconds). Please select authentication method again.':
    'OAuth-Token abgelaufen (uber {{seconds}} Sekunden). Bitte wahlen Sie erneut eine Authentifizierungsmethode.',
  'Press any key to return to authentication type selection.':
    'Drucken Sie eine beliebige Taste, um zur Authentifizierungstypauswahl zuruckzukehren.',
  'Waiting for Qwen OAuth authentication...':
    'Warten auf Qwen OAuth-Authentifizierung...',
  'Note: Your existing API key in settings.json will not be cleared when using Qwen OAuth. You can switch back to OpenAI authentication later if needed.':
    'Hinweis: Ihr bestehender API-Schlussel in settings.json wird bei Verwendung von Qwen OAuth nicht geloscht. Sie konnen spater bei Bedarf zur OpenAI-Authentifizierung zuruckwechseln.',
  'Authentication timed out. Please try again.':
    'Authentifizierung abgelaufen. Bitte versuchen Sie es erneut.',
  'Waiting for auth... (Press ESC or CTRL+C to cancel)':
    'Warten auf Authentifizierung... (ESC oder STRG+C zum Abbrechen drucken)',
  'Failed to authenticate. Message: {{message}}':
    'Authentifizierung fehlgeschlagen. Meldung: {{message}}',
  'Authenticated successfully with {{authType}} credentials.':
    'Erfolgreich mit {{authType}}-Anmeldedaten authentifiziert.',
  'Invalid QWEN_DEFAULT_AUTH_TYPE value: "{{value}}". Valid values are: {{validValues}}':
    'Ungultiger QWEN_DEFAULT_AUTH_TYPE-Wert: "{{value}}". Gultige Werte sind: {{validValues}}',
  'OpenAI Configuration Required': 'OpenAI-Konfiguration erforderlich',
  'Please enter your OpenAI configuration. You can get an API key from':
    'Bitte geben Sie Ihre OpenAI-Konfiguration ein. Sie konnen einen API-Schlussel erhalten von',
  'API Key:': 'API-Schlussel:',
  'Invalid credentials: {{errorMessage}}':
    'Ungultige Anmeldedaten: {{errorMessage}}',
  'Failed to validate credentials': 'Anmeldedaten konnten nicht validiert werden',
  'Press Enter to continue, Tab/↑↓ to navigate, Esc to cancel':
    'Enter zum Fortfahren, Tab/↑↓ zum Navigieren, Esc zum Abbrechen',

  // ============================================================================
  // Dialogs - Model
  // ============================================================================
  'Select Model': 'Modell auswahlen',
  '(Press Esc to close)': '(Esc zum Schliessen drucken)',
  'The latest Qwen Coder model from Alibaba Cloud ModelStudio (version: qwen3-coder-plus-2025-09-23)':
    'Das neueste Qwen Coder Modell von Alibaba Cloud ModelStudio (Version: qwen3-coder-plus-2025-09-23)',
  'The latest Qwen Vision model from Alibaba Cloud ModelStudio (version: qwen3-vl-plus-2025-09-23)':
    'Das neueste Qwen Vision Modell von Alibaba Cloud ModelStudio (Version: qwen3-vl-plus-2025-09-23)',

  // ============================================================================
  // Dialogs - Permissions
  // ============================================================================
  'Manage folder trust settings': 'Ordnervertrauenseinstellungen verwalten',

  // ============================================================================
  // Status Bar
  // ============================================================================
  'Using:': 'Verwendet:',
  '{{count}} open file': '{{count}} geoffnete Datei',
  '{{count}} open files': '{{count}} geoffnete Dateien',
  '(ctrl+g to view)': '(Strg+G zum Anzeigen)',
  '{{count}} {{name}} file': '{{count}} {{name}}-Datei',
  '{{count}} {{name}} files': '{{count}} {{name}}-Dateien',
  '{{count}} MCP server': '{{count}} MCP-Server',
  '{{count}} MCP servers': '{{count}} MCP-Server',
  '{{count}} Blocked': '{{count}} blockiert',
  '(ctrl+t to view)': '(Strg+T zum Anzeigen)',
  '(ctrl+t to toggle)': '(Strg+T zum Umschalten)',
  'Press Ctrl+C again to exit.': 'Drucken Sie erneut Strg+C zum Beenden.',
  'Press Ctrl+D again to exit.': 'Drucken Sie erneut Strg+D zum Beenden.',
  'Press Esc again to clear.': 'Drucken Sie erneut Esc zum Loschen.',

  // ============================================================================
  // MCP Status
  // ============================================================================
  'No MCP servers configured.': 'Keine MCP-Server konfiguriert.',
  'Please view MCP documentation in your browser:':
    'Bitte sehen Sie die MCP-Dokumentation in Ihrem Browser:',
  'or use the cli /docs command': 'oder verwenden Sie den CLI-Befehl /docs',
  '⏳ MCP servers are starting up ({{count}} initializing)...':
    '⏳ MCP-Server werden gestartet ({{count}} werden initialisiert)...',
  'Note: First startup may take longer. Tool availability will update automatically.':
    'Hinweis: Der erste Start kann langer dauern. Werkzeugverfugbarkeit wird automatisch aktualisiert.',
  'Configured MCP servers:': 'Konfigurierte MCP-Server:',
  Ready: 'Bereit',
  'Starting... (first startup may take longer)':
    'Wird gestartet... (erster Start kann langer dauern)',
  Disconnected: 'Getrennt',
  '{{count}} tool': '{{count}} Werkzeug',
  '{{count}} tools': '{{count}} Werkzeuge',
  '{{count}} prompt': '{{count}} Prompt',
  '{{count}} prompts': '{{count}} Prompts',
  '(from {{extensionName}})': '(von {{extensionName}})',
  OAuth: 'OAuth',
  'OAuth expired': 'OAuth abgelaufen',
  'OAuth not authenticated': 'OAuth nicht authentifiziert',
  'tools and prompts will appear when ready':
    'Werkzeuge und Prompts werden angezeigt, wenn bereit',
  '{{count}} tools cached': '{{count}} Werkzeuge zwischengespeichert',
  'Tools:': 'Werkzeuge:',
  'Parameters:': 'Parameter:',
  'Prompts:': 'Prompts:',
  Blocked: 'Blockiert',
  '💡 Tips:': '💡 Tipps:',
  Use: 'Verwenden',
  'to show server and tool descriptions':
    'um Server- und Werkzeugbeschreibungen anzuzeigen',
  'to show tool parameter schemas': 'um Werkzeug-Parameter-Schemas anzuzeigen',
  'to hide descriptions': 'um Beschreibungen auszublenden',
  'to authenticate with OAuth-enabled servers':
    'um sich bei OAuth-fahigen Servern zu authentifizieren',
  Press: 'Drucken Sie',
  'to toggle tool descriptions on/off':
    'um Werkzeugbeschreibungen ein-/auszuschalten',
  "Starting OAuth authentication for MCP server '{{name}}'...":
    "OAuth-Authentifizierung fur MCP-Server '{{name}}' wird gestartet...",
  'Restarting MCP servers...': 'MCP-Server werden neu gestartet...',

  // ============================================================================
  // Startup Tips
  // ============================================================================
  'Tips for getting started:': 'Tipps zum Einstieg:',
  '1. Ask questions, edit files, or run commands.':
    '1. Stellen Sie Fragen, bearbeiten Sie Dateien oder fuhren Sie Befehle aus.',
  '2. Be specific for the best results.':
    '2. Seien Sie spezifisch fur die besten Ergebnisse.',
  'files to customize your interactions with Qwen Code.':
    'Dateien, um Ihre Interaktionen mit Qwen Code anzupassen.',
  'for more information.': 'fur weitere Informationen.',

  // ============================================================================
  // Exit Screen / Stats
  // ============================================================================
  'Agent powering down. Goodbye!': 'Agent wird heruntergefahren. Auf Wiedersehen!',
  'To continue this session, run': 'Um diese Sitzung fortzusetzen, fuhren Sie aus',
  'Interaction Summary': 'Interaktionszusammenfassung',
  'Session ID:': 'Sitzungs-ID:',
  'Tool Calls:': 'Werkzeugaufrufe:',
  'Success Rate:': 'Erfolgsrate:',
  'User Agreement:': 'Benutzerzustimmung:',
  reviewed: 'uberpruft',
  'Code Changes:': 'Codeanderungen:',
  Performance: 'Leistung',
  'Wall Time:': 'Gesamtzeit:',
  'Agent Active:': 'Agent aktiv:',
  'API Time:': 'API-Zeit:',
  'Tool Time:': 'Werkzeugzeit:',
  'Session Stats': 'Sitzungsstatistiken',
  'Model Usage': 'Modellnutzung',
  Reqs: 'Anfragen',
  'Input Tokens': 'Eingabe-Token',
  'Output Tokens': 'Ausgabe-Token',
  'Savings Highlight:': 'Einsparungen:',
  'of input tokens were served from the cache, reducing costs.':
    'der Eingabe-Token wurden aus dem Cache bedient, was die Kosten reduziert.',
  'Tip: For a full token breakdown, run `/stats model`.':
    'Tipp: Fur eine vollstandige Token-Aufschlusselung fuhren Sie `/stats model` aus.',
  'Model Stats For Nerds': 'Modellstatistiken fur Nerds',
  'Tool Stats For Nerds': 'Werkzeugstatistiken fur Nerds',
  Metric: 'Metrik',
  API: 'API',
  Requests: 'Anfragen',
  Errors: 'Fehler',
  'Avg Latency': 'Durchschn. Latenz',
  Tokens: 'Token',
  Total: 'Gesamt',
  Prompt: 'Prompt',
  Cached: 'Zwischengespeichert',
  Thoughts: 'Gedanken',
  Tool: 'Werkzeug',
  Output: 'Ausgabe',
  'No API calls have been made in this session.':
    'In dieser Sitzung wurden keine API-Aufrufe gemacht.',
  'Tool Name': 'Werkzeugname',
  Calls: 'Aufrufe',
  'Success Rate': 'Erfolgsrate',
  'Avg Duration': 'Durchschn. Dauer',
  'User Decision Summary': 'Benutzerentscheidungs-Zusammenfassung',
  'Total Reviewed Suggestions:': 'Insgesamt uberprufter Vorschlage:',
  ' » Accepted:': ' » Akzeptiert:',
  ' » Rejected:': ' » Abgelehnt:',
  ' » Modified:': ' » Geandert:',
  ' Overall Agreement Rate:': ' Gesamtzustimmungsrate:',
  'No tool calls have been made in this session.':
    'In dieser Sitzung wurden keine Werkzeugaufrufe gemacht.',
  'Session start time is unavailable, cannot calculate stats.':
    'Sitzungsstartzeit nicht verfugbar, Statistiken konnen nicht berechnet werden.',

  // ============================================================================
  // Loading Phrases
  // ============================================================================
  'Waiting for user confirmation...': 'Warten auf Benutzerbestatigung...',
  '(esc to cancel, {{time}})': '(Esc zum Abbrechen, {{time}})',

  // ============================================================================
  // Loading Phrases
  // ============================================================================
  WITTY_LOADING_PHRASES: [
    'Auf gut Gluck!',
    'Genialitat wird ausgeliefert...',
    'Die Serifen werden aufgemalt...',
    'Durch den Schleimpilz navigieren...',
    'Die digitalen Geister werden befragt...',
    'Splines werden retikuliert...',
    'Die KI-Hamster werden aufgewarmt...',
    'Die Zaubermuschel wird befragt...',
    'Witzige Erwiderung wird generiert...',
    'Die Algorithmen werden poliert...',
    'Perfektion braucht Zeit (mein Code auch)...',
    'Frische Bytes werden gebruht...',
    'Elektronen werden gezahlt...',
    'Kognitive Prozessoren werden aktiviert...',
    'Auf Syntaxfehler im Universum wird gepruft...',
    'Einen Moment, Humor wird optimiert...',
    'Pointen werden gemischt...',
    'Neuronale Netze werden entwirrt...',
    'Brillanz wird kompiliert...',
    'wit.exe wird geladen...',
    'Die Wolke der Weisheit wird beschworen...',
    'Eine witzige Antwort wird vorbereitet...',
    'Einen Moment, ich debugge die Realitat...',
    'Die Optionen werden verwirrt...',
    'Kosmische Frequenzen werden eingestellt...',
    'Eine Antwort wird erstellt, die Ihrer Geduld wurdig ist...',
    'Die Einsen und Nullen werden kompiliert...',
    'Abhangigkeiten werden aufgelost... und existenzielle Krisen...',
    'Erinnerungen werden defragmentiert... sowohl RAM als auch personliche...',
    'Das Humor-Modul wird neu gestartet...',
    'Das Wesentliche wird zwischengespeichert (hauptsachlich Katzen-Memes)...',
    'Fur lacherliche Geschwindigkeit wird optimiert',
    'Bits werden getauscht... sagen Sie es nicht den Bytes...',
    'Garbage Collection lauft... bin gleich zuruck...',
    'Das Internet wird zusammengebaut...',
    'Kaffee wird in Code umgewandelt...',
    'Die Syntax der Realitat wird aktualisiert...',
    'Die Synapsen werden neu verdrahtet...',
    'Ein verlegtes Semikolon wird gesucht...',
    'Die Zahnrader werden geschmiert...',
    'Die Server werden vorgeheizt...',
    'Der Fluxkompensator wird kalibriert...',
    'Der Unwahrscheinlichkeitsantrieb wird aktiviert...',
    'Die Macht wird kanalisiert...',
    'Die Sterne werden fur optimale Antwort ausgerichtet...',
    'So sagen wir alle...',
    'Die nachste grosse Idee wird geladen...',
    'Einen Moment, ich bin in der Zone...',
    'Bereite mich vor, Sie mit Brillanz zu blenden...',
    'Einen Augenblick, ich poliere meinen Witz...',
    'Halten Sie durch, ich erschaffe ein Meisterwerk...',
    'Einen Moment, ich debugge das Universum...',
    'Einen Moment, ich richte die Pixel aus...',
    'Einen Moment, ich optimiere den Humor...',
    'Einen Moment, ich tune die Algorithmen...',
    'Warp-Geschwindigkeit aktiviert...',
    'Mehr Dilithium-Kristalle werden gesucht...',
    'Keine Panik...',
    'Dem weissen Kaninchen wird gefolgt...',
    'Die Wahrheit ist hier drin... irgendwo...',
    'Auf die Kassette wird gepustet...',
    'Ladevorgang... Machen Sie eine Fassrolle!',
    'Auf den Respawn wird gewartet...',
    'Der Kessel-Flug wird in weniger als 12 Parsec beendet...',
    'Der Kuchen ist keine Luge, er ladt nur noch...',
    'Am Charaktererstellungsbildschirm wird herumgefummelt...',
    'Einen Moment, ich suche das richtige Meme...',
    "'A' wird zum Fortfahren gedruckt...",
    'Digitale Katzen werden gehuttert...',
    'Die Pixel werden poliert...',
    'Ein passender Ladebildschirm-Witz wird gesucht...',
    'Ich lenke Sie mit diesem witzigen Spruch ab...',
    'Fast da... wahrscheinlich...',
    'Unsere Hamster arbeiten so schnell sie konnen...',
    'Cloudy wird am Kopf gestreichelt...',
    'Die Katze wird gestreichelt...',
    'Meinen Chef rickrollen...',
    'Never gonna give you up, never gonna let you down...',
    'Auf den Bass wird geschlagen...',
    'Die Schnozbeeren werden probiert...',
    "I'm going the distance, I'm going for speed...",
    'Ist dies das wahre Leben? Ist dies nur Fantasie?...',
    'Ich habe ein gutes Gefuhl dabei...',
    'Den Baren wird gestupst...',
    'Recherche zu den neuesten Memes...',
    'Uberlege, wie ich das witziger machen kann...',
    'Hmmm... lassen Sie mich nachdenken...',
    'Wie nennt man einen Fisch ohne Augen? Ein Fsh...',
    'Warum ging der Computer zur Therapie? Er hatte zu viele Bytes...',
    'Warum mogen Programmierer keine Natur? Sie hat zu viele Bugs...',
    'Warum bevorzugen Programmierer den Dunkelmodus? Weil Licht Bugs anzieht...',
    'Warum ging der Entwickler pleite? Er hat seinen ganzen Cache aufgebraucht...',
    'Was kann man mit einem kaputten Bleistift machen? Nichts, er ist sinnlos...',
    'Perkussive Wartung wird angewendet...',
    'Die richtige USB-Ausrichtung wird gesucht...',
    'Es wird sichergestellt, dass der magische Rauch in den Kabeln bleibt...',
    'Versuche Vim zu beenden...',
    'Das Hamsterrad wird angeworfen...',
    'Das ist kein Bug, das ist ein undokumentiertes Feature...',
    'Engage.',
    'Ich komme wieder... mit einer Antwort.',
    'Mein anderer Prozess ist eine TARDIS...',
    'Mit dem Maschinengeist wird kommuniziert...',
    'Die Gedanken marinieren lassen...',
    'Gerade erinnert, wo ich meine Schlussel hingelegt habe...',
    'Uber die Kugel wird nachgedacht...',
    'Ich habe Dinge gesehen, die Sie nicht glauben wurden... wie einen Benutzer, der Lademeldungen liest.',
    'Nachdenklicher Blick wird initiiert...',
    'Was ist der Lieblingssnack eines Computers? Mikrochips.',
    'Warum tragen Java-Entwickler Brillen? Weil sie nicht C#.',
    'Der Laser wird aufgeladen... pew pew!',
    'Durch Null wird geteilt... nur Spass!',
    'Suche nach einem erwachsenen Aufseh... ich meine, Verarbeitung.',
    'Es piept und boopt.',
    'Pufferung... weil auch KIs einen Moment brauchen.',
    'Quantenteilchen werden fur schnellere Antwort verschrankt...',
    'Das Chrom wird poliert... an den Algorithmen.',
    'Sind Sie nicht unterhalten? (Arbeite daran!)',
    'Die Code-Gremlins werden beschworen... zum Helfen, naturlich.',
    'Warte nur auf das Einwahlton-Ende...',
    'Das Humor-O-Meter wird neu kalibriert.',
    'Mein anderer Ladebildschirm ist noch lustiger.',
    'Ziemlich sicher, dass irgendwo eine Katze uber die Tastatur lauft...',
    'Verbessern... Verbessern... Ladt noch.',
    'Das ist kein Bug, das ist ein Feature... dieses Ladebildschirms.',
    'Haben Sie versucht, es aus- und wieder einzuschalten? (Den Ladebildschirm, nicht mich.)',
    'Zusatzliche Pylonen werden gebaut...',
  ],
};
