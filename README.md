# ⚡ DevDrive (v0.1.0)
> **Developer-Aware Smart Virtual Filesystem & Tiered Caching for macOS**

DevDrive, 256 GB yerel SSD'ye sahip geliştiricilerin 5 TB Google Drive gibi bulut sürücülerini kilitlenme, CPU patlaması veya I/O gecikmesi yaşamadan birincil geliştirme alanı gibi kullanabilmesini sağlayan akıllı bir dosya sistemi ve önbellek motorudur.

---

## 🚀 Öne Çıkan Yetenekler

1. **🛡️ Dev-Shield Filtreleme Motoru:**
   - `node_modules`, `target`, `.venv`, `.next`, `DerivedData`, `.turbo`, `.git/index.lock` gibi milyonlarca geçici/türetilebilir dosyayı tespit eder.
   - Bu dosyaları yerel SSD'deki geçici havuzda tutar; Google Drive'a **asla** göndermez. Sadece saf kaynak kodları eşitlenir.

2. **💾 Çok Katmanlı Önbellek (Multi-Tier LRU Cache):**
   - Sabit 20 GB yerel "Sıcak SSD" önbellek havuzu.
   - Sık kullanılan dosyalar NVMe SSD hızında (0 ms) açılır, eskiyen dosyalar otomatik tahliye edilir.
   - Yerel diskiniz asla 256 GB sınırına dayanmaz.

3. **📸 Git Bağımsız Zaman Makinesi (Micro-Snapshots & Rollback):**
   - Git commit atmasanız bile dilediğiniz an projenin anlık görüntüsünü (Content-Addressable Storage) kaydeder.
   - Tek komutla saniyeler içinde eski bir koda geri sarabilirsiniz.

4. **⚡ macOS FSEvents & Write-Ahead Log (WAL):**
   - Dosya değişikliklerini Apple'ın yerel `FSEvents` API'si ile dinler.
   - Yazma işlemlerini akıllıca birleştirir (debounce/batch), Google Drive API kota sınırlarını aşmaz.
   - İnternet kopsa bile yerel WAL kuyruğunda bekletir, bağlantı gelince sessizce eşitler.

5. **🔍 Otomatik Google Drive Keşfi:**
   - macOS üzerindeki CloudStorage yolunu (`~/Library/CloudStorage/GoogleDrive-.../Drive'ım/DevDrive`) otomatik tespit eder.

---

## 🛠️ Kurulum & Kullanım

`devdrive` doğrudan Mac terminalinize global olarak kurulmuştur:

```bash
# Durum ve disk tüketimini görüntüle
devdrive status

# Bir projeyi DevDrive korumasına al ve ön ısıtma yap
devdrive init /path/to/project --name my-project

# Projeyi gerçek zamanlı izlemeye başla (FSEvents daemon)
devdrive watch /path/to/project

# Git'ten bağımsız anlık mikro-görüntü (Snapshot) al
devdrive snapshot /path/to/project -m "Refactor öncesi durum"

# Alınan anlık görüntüleri listele
devdrive list-snapshots /path/to/project

# Projeyi önceki bir anlık görüntüye geri sar
devdrive rollback /path/to/project <snapshot_id>

# Değişiklikleri Google Drive'a eşitle
devdrive sync /path/to/project

# Tüm temiz kaynak kodları tek seferde Google Drive'a yükle
devdrive sync /path/to/project --all

# Önbellek temizliği yap
devdrive cache-clean
```

---

## 📁 Mimari Katmanları

* `src/filter.rs`: Geliştirici çöp kalkanı (Kaynak vs. Ephemeral ayrımı).
* `src/db.rs`: SQLite WAL modunda milisaniyelik metadata ve snapshot indeksi.
* `src/cache.rs`: APFS zero-copy clonefile ve CAS (Content-Addressable Storage) havuzu.
* `src/snapshot.rs`: Anlık görüntü yakalama ve rollback motoru.
* `src/wal.rs`: Çevrimdışı ve kuyruklu senkronizasyon (Write-Ahead Log).
* `src/watcher.rs`: macOS FSEvents tabanlı debounced dinleyici.
* `src/sync.rs`: Google Drive eşitleme ve predictive warm cache motoru.
* `src/config.rs`: Dinamik macOS CloudStorage tespiti ve konfigürasyon.
