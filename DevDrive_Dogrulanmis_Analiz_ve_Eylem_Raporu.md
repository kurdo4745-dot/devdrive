# DevDrive (`adventurous-hypatia`) Doğrulanmış Mühendislik Analizi ve Eylem Raporu

* **Proje Dizini:** [adventurous-hypatia](https://drive.google.com/drive/folders/14Fiqo-xwrp4pvdWFqdNfCrNBnIMDmELB)  
* **Kaynak Kod Dizini:** [src/](https://drive.google.com/drive/folders/1UkraVpHwCifTgYFbcefDRDMQjvsSBCW9)  
* **Karşılaştırılan Doküman:** [DEVDRIVE\_ANALYSIS\_REPORT.md](https://drive.google.com/file/d/1d9262v9TWmPtSO0IsyyZECoh2AlALWuJ/view?usp=drivesdk)

Bu rapor, [DEVDRIVE\_ANALYSIS\_REPORT.md](https://drive.google.com/file/d/1d9262v9TWmPtSO0IsyyZECoh2AlALWuJ/view?usp=drivesdk) içinde yer alan 17 bulgunun [adventurous-hypatia/src](https://drive.google.com/drive/folders/1UkraVpHwCifTgYFbcefDRDMQjvsSBCW9) altındaki güncel kaynak kodlarla karşılaştırılması sonucunda hazırlanmıştır. Amaç; **gerçekten düzeltilmesi gereken kritik noktaları** doğrudan uygulanabilir kod yamalarıyla sunmak, **veri kaybına veya aşırı mühendisliğe (overengineering) yol açacak hatalı önerileri** ise elemektir.

## 1\. Yönetici Özeti: 17 Bulgunun Mühendislik Sınıflandırması

| Kategori | Rapor Maddeleri | Karar ve Gerekçe |
| :---- | :---- | :---- |
| **1\. Gerçekten Düzeltilmesi Gerekenler (5 Madde)** | **Madde 4** (`.devdriveignore` bağlantısı), **Madde 7** (`safe_sync_file` boyut/`mtime` kontrolü), **Madde 8** (Atomik `unarchive`), **Madde 9** (`include_git` koruması), **Madde 13 & 17** (`open -R` ve UI tırnak kaçışı) | **Uygulanmalı:** Doğrudan performansı, `.devdriveignore` işlevselliğini ve hata toleransını iyileştirir. |
| **2\. Reddedilen / Mimari Tercihle Çelişenler (6 Madde)** | **Madde 2 & 5** (CSRF Token), **Madde 3** (`recipe.json` RCE iddiası), **Madde 6** (`.env` arşivden çıkarma önerisi), **Madde 12** (`LaunchAgent` watch), **Madde 14** (SQLite bağlantı havuzu) | **Uygulanmamalı:** Madde 6 uygulanırsa `purge` sonrası `.env` dosyaları kalıcı olarak silinir. Diğer maddeler yerel (`127.0.0.1`) geliştirici aracı için gereksiz karmaşıklık yaratır. |
| **3\. Düşük Öncelikli Hijyen İyileştirmeleri (6 Madde)** | **Madde 1** (AppleScript `argv`), **Madde 10** (Polling süresi), **Madde 11** (CAS Pinning), **Madde 15** (`wal_events` temizliği), **Madde 16** (Çoklu dil Drive), **Clippy** uyarıları | **İsteğe Bağlı:** Sistemin çalışmasını engellemez; bakım aşamasında hızlıca eklenebilir. |

## 2\. Uygulanması Önerilen 5 Gerçek Bulgu ve Kod Yamaları

### 2.1. `.devdriveignore` (`DevFilter::for_project`) Motor Bağlantılarının Tamamlanması (Rapor Madde 4\)

* **İlgili Dosyalar:** [src/filter.rs](https://drive.google.com/file/d/16Uxit8uxIs0VAHSJqohLJtZ2yYuBL1l9/view?usp=drivesdk), [src/snapshot.rs](https://drive.google.com/file/d/14sFMi79SeMkt0yYZbSN4gauXydpVc_M3/view?usp=drivesdk), [src/sync.rs](https://drive.google.com/file/d/1CAbj6zCjFwIVBgQEiUoORxYxeZmn31Mk/view?usp=drivesdk), [src/watcher.rs](https://drive.google.com/file/d/137WpO0kIYSpfZCuRa9ibGj4BXYx6pKTH/view?usp=drivesdk), [src/ui.rs](https://drive.google.com/file/d/1TgCXhKRyN4aPgNBqgzR7gV-Tn7oCthtG/view?usp=drivesdk), [src/main.rs](https://drive.google.com/file/d/1R-PVeNOihoD8Jt9mN2fheC0VVEjnIP5r/view?usp=drivesdk)  
* **Teknik Açıklama:** [src/filter.rs](https://drive.google.com/file/d/16Uxit8uxIs0VAHSJqohLJtZ2yYuBL1l9/view?usp=drivesdk) içinde `.devdriveignore` kurallarını okuyan `DevFilter::for_project(project_root)` metodu eksiksiz yazılmış ve sadece `diff_snapshot` içinde kullanılmış. Ancak `create_snapshot`, `rollback_to_snapshot`, `sync_all_with_options`, `prune_remote`, `WorkspaceWatcher::new`, `Commands::Init` ve `get_status` içinde hâlâ proje yolunu bilmeyen `DevFilter::new()` çağrılıyor.  
* **Çözüm:** Proje kök dizininin bilindiği bu noktalarda `DevFilter::new()` yerine `DevFilter::for_project(&project_path)` çağrılmalıdır:

// 1\. src/watcher.rs (satır 19-25):

impl WorkspaceWatcher {

    pub fn new\<P: AsRef\<Path\>\>(project\_dir: P) \-\> Result\<Self\> {

        let canonical \= project\_dir.as\_ref().canonicalize()?;

        let filter \= DevFilter::for\_project(\&canonical);

        Ok(Self {

            project\_dir: canonical,

            filter,

        })

    }

// 2\. src/snapshot.rs içinde \`create\_snapshot\` ve \`rollback\_to\_snapshot\` başında:

let project\_path \= project\_path.as\_ref().canonicalize()?;

self.filter \= DevFilter::for\_project(\&project\_path);

// 3\. src/sync.rs içinde:

// \- list\_remote\_projects (satır 124): \`let filter \= DevFilter::for\_project(\&path);\`

// \- sync\_all\_with\_options (satır 494): \`let filter \= DevFilter::for\_project(\&local\_path);\`

// \- prune\_remote (satır 610): \`let filter \= DevFilter::for\_project(\&local\_path);\`

// \- restore\_from\_remote (satır 740): \`let filter \= DevFilter::for\_project(\&canonical\_remote);\`

// 4\. src/main.rs (Commands::Init içinde) ve src/ui.rs (get\_status ve simulate\_handler içinde):

// \`let filter \= DevFilter::new();\` yerine:

let filter \= DevFilter::for\_project(\&p\_path); // (Init içinde \&canonical)

### 2.2. `safe_sync_file` İçinde Hızlı Boyut ve `mtime` Kontrolü — Stat-First Sync (Rapor Madde 7\)

* **İlgili Dosya:** [src/sync.rs](https://drive.google.com/file/d/1CAbj6zCjFwIVBgQEiUoORxYxeZmn31Mk/view?usp=drivesdk)  
* **Teknik Açıklama:** `safe_sync_file` fonksiyonu her dosya için önce yerel dosyanın, ardından hedef Drive dosyasının (`dst_file`) tamamını okuyarak SHA-256 hesaplıyor. Oysa `safe_sync_file` zaten kopyalama sonrasında yerel dosyanın `mtime` değerini Drive dosyasına eşitliyor (`filetime::set_file_mtime`). Değişmemiş binlerce dosyada Google Drive üzerinden tam dosya okuması yapmak senkronizasyonu yavaşlatır.  
* **Çözüm:** Hedef dosya mevcutsa önce boyut (`len`) ve değiştirilme zamanı (`mtime`) karşılaştırılmalı; ikisi de aynıysa dosya içeriği hiç okunmadan `Ok(SyncFileResult::Unchanged)` dönülmelidir:

// src/sync.rs içinde \`safe\_sync\_file\` fonksiyonunun başı (satır 198-215):

    pub fn safe\_sync\_file(src\_file: \&Path, dst\_file: \&Path) \-\> Result\<SyncFileResult\> {

        if \!src\_file.exists() || \!src\_file.is\_file() {

            return Ok(SyncFileResult::Unchanged);

        }

        if let Some(parent) \= dst\_file.parent() {

            std::fs::create\_dir\_all(parent)?;

        }

        // Hızlı Stat Kontrolü: Boyut ve mtime (saniye bazında) aynıysa SHA-256 hesaplamadan atla

        if dst\_file.exists() {

            if let (Ok(meta\_src), Ok(meta\_dst)) \= (src\_file.metadata(), dst\_file.metadata()) {

                if meta\_src.len() \== meta\_dst.len() {

                    let mtime\_src \= filetime::FileTime::from\_last\_modification\_time(\&meta\_src);

                    let mtime\_dst \= filetime::FileTime::from\_last\_modification\_time(\&meta\_dst);

                    if mtime\_src.seconds() \== mtime\_dst.seconds() {

                        return Ok(SyncFileResult::Unchanged);

                    }

                }

            }

        }

        let src\_hash \= CacheManager::compute\_sha256(src\_file)?;

        // ... mevcut conflict ve kopyalama akışı aynen devam eder ...

### 2.3. Atomik Olmayan `unarchive` Hatasında Yarım Kalan Klasörün Temizlenmesi (Rapor Madde 8\)

* **İlgili Dosya:** [src/offload.rs](https://drive.google.com/file/d/1ob3ErjIZVw9ccNA9RA3wFLDOjPjjyJnC/view?usp=drivesdk)  
* **Teknik Açıklama:** `unarchive` fonksiyonu `tree.tar.gz` arşivini açarken doğrudan `target_local_path` klasörünü oluşturuyor. Arşiv açılırken veya sonrasında SHA-256 doğrulaması yapılırken bir hata oluşursa (`?` veya `bail!`), yarım kalan `target_local_path` diskte kalıyor ve kullanıcı komutu tekrar çalıştırdığında `"Hedef yerel dizin zaten mevcut"` hatasıyla karşılaşıyor.  
* **Çözüm:** Arşivden geri yükleme adımı bir alt blokta çalıştırılıp hata döndüğünde oluşturulan yarım dizin otomatik olarak temizlenmelidir:

// src/offload.rs içinde \`unarchive\` fonksiyonunda Drive arşivinden geri yükleme bloğu:

        let restore\_result: Result\<(usize, u64)\> \= (|| {

            std::fs::create\_dir\_all(\&target\_local\_path)?;

            // ... mevcut tar.gz veya tree/ kopyalama ve SHA-256 doğrulama döngüsü ...

            Ok((files\_restored, bytes\_restored))

        })();

        let (files\_restored, bytes\_restored) \= match restore\_result {

            Ok(res) \=\> res,

            Err(e) \=\> {

                // Hata durumunda yarım kalan hedef klasörü temizle ki tekrar denenebilsin

                let \_ \= std::fs::remove\_dir\_all(\&target\_local\_path);

                return Err(e);

            }

        };

### 2.4. `reveal_in_finder` (`open -R`) ve Dashboard Tek Tırnak Kaçışı (Rapor Madde 13 & 17\)

* **İlgili Dosya:** [src/ui.rs](https://drive.google.com/file/d/1TgCXhKRyN4aPgNBqgzR7gV-Tn7oCthtG/view?usp=drivesdk)  
* **Teknik Açıklama:** `reveal_in_finder_handler` içinde `open <path>` çağrıldığı için hedef yol bir betik veya uygulama ise Finder'da gösterilmek yerine çalıştırılabilir. `-R` bayrağı ise öğeyi Finder içinde güvenli şekilde seçer.  
* **Çözüm:** [src/ui.rs](https://drive.google.com/file/d/1TgCXhKRyN4aPgNBqgzR7gV-Tn7oCthtG/view?usp=drivesdk) içindeki `reveal_in_finder_handler` fonksiyonunda `-R` bayrağı kullanılmalıdır:

// src/ui.rs içinde \`reveal\_in\_finder\_handler\`:

\#\[cfg(target\_os \= "macos")\]

{

    let \_ \= std::process::Command::new("open")

        .arg("-R")

        .arg(\&payload.path)

        .spawn();

}

### 2.5. `offload` Sırasında `.git` Geçmişi Koruması (Rapor Madde 9\)

* **İlgili Dosya:** [src/offload.rs](https://drive.google.com/file/d/1ob3ErjIZVw9ccNA9RA3wFLDOjPjjyJnC/view?usp=drivesdk)  
* **Teknik Açıklama:** `include_git` varsayılan olarak `true` olduğu için normalde `.git` klasörü `tree.tar.gz` arşivine dahil edilir. Ancak kullanıcı yapılandırmada `include_git = false` yaparsa, `offload` sonrasında `purge` çalıştırıldığında yerel `.git` klasörü de silineceği için commit geçmişi kaybolabilir.  
* **Çözüm:** `scan_project` içinde `include_git == false` iken projede `.git` dizini varsa kullanıcıya açık bir uyarı mesajı gösterilmeli veya `offload` sırasında `.git` her zaman korunmalıdır.

## 3\. Neden Uygulanmamalı? — Reddedilen 6 Bulgunun Teknik Gerekçesi

1. **Rapor Madde 6 (`.env` Dosyalarını Arşivden Çıkarma Önerisi):** Raporda `.env` dosyalarının `tree.tar.gz` arşivine konulmaması ve sadece yerel karantinada tutulması önerilmiş. **Bu öneri uygulanırsa veri kaybı yaşanır:** 7 günlük karantina süresi dolup `devdrive purge` çalıştırıldığında yerel karantina silinir; eğer `.env` dosyaları Drive arşivinde (`tree.tar.gz`) yoksa projeyi geri yüklediğinizde (`unarchive`) tüm `.env` dosyalarınız kalıcı olarak kaybolmuş olur. Mevcut tasarımınız (günlük `sync` işleminde `.env`'i hariç tutup, projeyi diskten kaldıran `offload` arşivine dahil etmek) doğrudur.  
2. **Rapor Madde 2 & Madde 5 (Web UI CSRF Token & `/api/simulate`):** Sunucu dış ağa kapalıdır ve yalnızca `127.0.0.1` üzerinde geliştirici tarafından ihtiyaç duyulduğunda çalıştırılmaktadır. Tam kapsamlı kriptografik oturum/token yönetimi eklemek aşırı mühendisliktir.  
3. **Rapor Madde 3 (`recipe.json` Üzerinden Keyfi Komut Çalıştırma):** Kullanıcının kendi Google Drive hesabındaki arşiv klasörüne yazma yetkisi olan bir saldırgan zaten kaynak kodları (`Cargo.toml`, `build.rs`, `package.json`) da değiştirebilir.  
4. **Rapor Madde 12 (`LaunchAgent` Servisinin Otomatik `watch` Yapmaması):** Yarım kalan derlemelerde veya her dosya kaydında ağ trafiğini boğmamak için eşitlemenin kontrollü/manuel yapılması bilinçli bir mimari tercihtir.  
5. **Rapor Madde 14 (Her İstek İçin SQLite Bağlantı Havuzu):** Tek kullanıcılı yerel arayüzde `PRAGMA busy_timeout = 5000;` ve WAL modu fazlasıyla yeterlidir; harici connection pool kütüphaneleri eklemek gereksiz bağımlılık yaratır.

## 4\. İsteğe Bağlı Hafif Hijyen İyileştirmeleri

* **Eşitlenen WAL Kayıtlarının Temizlenmesi (Rapor Madde 15):** [src/db.rs](https://drive.google.com/file/d/1L4gD-txMUjBJEs6nnEnEn8hSLxCMqMla/view?usp=drivesdk) içindeki `mark_wal_events_synced` fonksiyonunda kayıtları `UPDATE wal_events SET synced = 1` yapmak yerine doğrudan `DELETE FROM wal_events WHERE id IN (...)` ile silmek, veritabanının zamanla şişmesini tek satırda önler (çünkü `synced = 1` olan kayıtlar bir daha hiçbir yerde kullanılmamaktadır).  
* **AppleScript `argv` Kullanımı (Rapor Madde 1):** [src/ui.rs](https://drive.google.com/file/d/1TgCXhKRyN4aPgNBqgzR7gV-Tn7oCthtG/view?usp=drivesdk) içindeki `pick_folder_handler` fonksiyonunda `prompt` değişkenini doğrudan AppleScript metnine gömmek yerine `on run argv` ile argüman olarak geçmek ters bölü (`\`) karakterlerinde oluşabilecek sözdizimi hatasını önler.  
* **Clippy Temizliği:** `cargo clippy --fix --allow-dirty` komutuyla `collapsible_if` uyarıları tek seferde otomatik temizlenebilir.