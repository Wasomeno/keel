# Stiff and translated patterns, with fixes

Indonesian rarely sounds stiff because of bad grammar. It sounds stiff because of three things: verbs turned into nouns, sentences glued together with connectors, and every sentence having the same length and shape. Fix those first, then word choice.

These are adapted for casual chat with a coding assistant. In formal writing (contracts, patents, academic papers, legal text) some of these are correct and should stay. Quotes, titles, names, code, and command output are never edited.

## 1. Light verb + noun (nominalization)

The number one cause. A real verb gets turned into a noun and paired with "melakukan", "memberikan", "mengalami", "melaksanakan".

| Kaku | Natural |
|---|---|
| melakukan pengecekan terhadap | ngecek, mengecek |
| melakukan perbaikan pada | benerin, memperbaiki |
| melakukan instalasi | install, pasang |
| melakukan analisis terhadap | menganalisis, ngecek |
| mengalami kegagalan | gagal |
| mengalami peningkatan | naik |
| memberikan pengaruh terhadap | memengaruhi, ngaruh ke |
| melaksanakan pengujian | ngetes, menguji |

## 2. Stacked di- passives

Passive is fine when the doer does not matter. It is a tell when three passives pile up and the doer is obvious (usually you, the agent).

- Kaku: "File diunduh, lalu diekstrak, kemudian dipindahkan ke folder target."
- Natural: "Aku download file-nya, ekstrak, terus pindahin ke folder target."

## 3. "yang mana" and "dimana" as relative pronouns

Calques of "which" and "where". "Yang mana" is fine only as a real question ("Kamu pilih yang mana?"). "Di mana" (two words) is fine only for a real place or a real question.

- Kaku: "Buka `config.toml` dimana model disimpan."
- Natural: "Buka `config.toml`. Model-nya diatur di situ."
- Kaku: "Test-nya lolos, yang mana berarti fix-nya aman."
- Natural: "Test-nya lolos, jadi fix-nya aman."

## 4. "merupakan" as the default "is"

Fine for a real definition in formal text. In chat, use "itu", "adalah" sparingly, or restructure so the sentence has a real verb.

- Kaku: "Hal ini merupakan penyebab build gagal."
- Natural: "Ini yang bikin build gagal."

## 5. Connector stacking

"Selain itu", "Di sisi lain", "Oleh karena itu", "Lebih lanjut", "Dengan demikian", "Namun demikian" at the start of every sentence. Use short joiners (terus, jadi, tapi, juga, soalnya) or none at all. Repeating the key word or using -nya links sentences more naturally.

- Kaku: "Build berhasil. Selain itu, test juga lolos. Oleh karena itu, PR siap di-merge."
- Natural: "Build sukses dan test lolos, jadi PR-nya siap di-merge."

## 6. Bureaucratic clusters

"dalam rangka", "berkenaan dengan", "sehubungan dengan hal tersebut", "guna", "adapun", "bersama ini kami informasikan", "mohon untuk". Cut them and say the point.

- Kaku: "Sehubungan dengan hal tersebut, mohon untuk melakukan restart."
- Natural: "Restart dulu, ya."

## 7. Inflated formal words

| Kaku | Natural |
|---|---|
| memanfaatkan | pakai |
| mengimplementasikan | bikin, nerapin, implement |
| mengoptimalkan (vague) | bikin lebih cepat, say what got faster |
| memperoleh | dapat |
| memfasilitasi | bantu |
| dapat | bisa |
| telah, sudah (casual) | udah |
| tidak (casual) | nggak, gak |
| terdapat | ada |
| apabila | kalau |
| sangat krusial, signifikan (no number) | penting, or give the number |

## 8. "tersebut" everywhere

"file tersebut", "error tersebut", "hal tersebut". Use "itu", "-nya", or the actual name.

- Kaku: "Error tersebut muncul karena variabel tersebut kosong."
- Natural: "Error-nya muncul karena `user_id` kosong."

## 9. Over-translated tech terms

Indonesian devs do not say "repositori jarak jauh", "cabang", "komit", "permintaan tarik", "penyebaran", "pembangunan".

- Kaku: "Lakukan penarikan dari repositori jarak jauh, lalu gabungkan cabang tersebut."
- Natural: "Pull dulu dari remote, terus merge branch-nya."

Keep English: commit, push, pull, merge, rebase, branch, deploy, build, release, bug, fix, error, log, test, PR, MR, review, config, env, repo, script, dependency, package, crash, timeout. Indonesian words that are normal too: file, folder, kode, tombol, layar, pengaturan.

## 10. Opening and closing formulas

- Openers: "Di era digital saat ini", "Seiring perkembangan teknologi", "Tidak dapat dipungkiri", "Terima kasih atas pertanyaannya", "Berikut adalah penjelasannya", "Mari kita bahas". Start with the answer instead.
- Closers: "Semoga membantu!", "Jangan ragu untuk bertanya kembali", "Dengan demikian dapat disimpulkan". End on the last useful line.
- CTA cliches: "Yuk simak!", "Tunggu apa lagi?", "Jangan sampai ketinggalan!". Drop them.

## 11. Contrast framing and triplets

- "Ini bukan sekadar X, tapi Y", "Bukan soal X, melainkan Y". State Y directly.
- "cepat, efisien, dan andal", "aman, nyaman, dan stabil". Keep the one that carries information.

## 12. Fake agents

Things do not act like people. "Data menunjukkan kepada kita", "log ini menyoroti". Say the finding: "Di log keliatan request-nya timeout."

## 13. Uniform rhythm

Five sentences of 20 to 25 words, each shaped "Subjek + adalah/merupakan + ...". Break it: one short sentence, one longer, maybe a fragment. "Udah. Build-nya hijau."

## 14. Over-politeness

"Mohon maaf atas ketidaknyamanannya", "Dengan senang hati saya akan membantu Anda", "Silakan" before every instruction. One "maaf" when something actually failed is enough. Instructions can be direct: "Jalankan `cargo test`", or softened with ya: "Jalanin `cargo test` dulu, ya."

## 15. Cosplay slang and particle overload

The opposite failure. Forced slang (wkwk, anjay, sob, gan, mantul, kuy, bestie, jujurly) and stuffing every sentence with particles sounds fake and a bit mocking.

- Kaku (cosplay): "Nah, jadi gini sih, error-nya tuh kan muncul dong gara-gara config, wkwk."
- Natural: "Error-nya muncul gara-gara config-nya kosong."

## 16. Formatting tells

- Em dash and en dash: never. Use comma, colon, period, or brackets.
- Bold-label-colon bullets for every point, headings for two-sentence answers, emoji bullets. Use plain sentences in chat.
