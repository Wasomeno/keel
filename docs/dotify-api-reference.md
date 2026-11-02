# Dotify API Reference Specification (For AI Agents & Automation)

Dokumentasi spesifikasi lengkap API backend Dotify untuk AI Agent, bot automasi, dan developer tool. Berisi rincian skema request, header, payload, respons nyata, dan aturan invarian arsitektur backend.

---

## 1. Lingkungan & Base URL

| Environment | Base URL | Catatan |
| :--- | :--- | :--- |
| **Production** | `https://api.dotify.dot.co.id/api/v1` | Server produksi aktif |
| **Development** | `https://api.dotify-dev.dot.co.id/api/v1` | Server staging/development |

> **Global Prefix**: Seluruh rute diawali dengan `/api/v1`.  
> **Konfigurasi Lokal CLI**: Tersimpan di `~/.config/dotify/config.json`.

---

## 2. Header & Skema Autentikasi

Semua request wajib menyertakan header standar berikut:

```http
Authorization: Bearer <JWT_ACCESS_TOKEN>
Content-Type: application/json
Accept: application/json
User-Agent: Dotify-CLI/1.0
```

### Format Amplop Respons (Standard API Response Wrapper)

Mayoritas respons API Dotify dibungkus dalam amplop standar:

```json
{
  "message": "string",
  "data": T | null
}
```

---

## 3. Aturan Invarian Backend (PENTING untuk AI Agent)

1. **`startDate` Bersifat Permanen / Tidak Bisa Diubah (`PUT /time-entities/:id`)**  
   Endpoint `PUT /time-entities/:id` **hanya menerima `endDate`, `projectId`, dan `description`**. Field `startDate` tidak didukung oleh DTO backend. Jam mulai entri selalu terkunci dari jam clock-in riil atau jam default bulk create (`08:00`).
2. **Tidak Ada Endpoint `POST /time-entities` (Custom Past Time Log)**  
   Request `POST /api/v1/time-entities` menghasilkan `404 Not Found`. Pembuatan log lampau diizinkan hanya melalui `POST /api/v1/time-entities/bulk-create-by-dates` (jam awal di-hardcode ke `08:00`), lalu `endDate`-nya disesuaikan via `PUT`.
3. **Tidak Ada Endpoint `DELETE /time-entities/:id`**  
   Controller `TimeEntityController` tidak mengekspos method `@Delete`. Jika ingin meniadakan jam kerja suatu log yang keliru, ubah `endDate` sama dengan `startDate` agar durasinya menjadi `0h`.
4. **Attendance Confirmation Lock**  
   Log waktu yang berada dalam periode absensi berstatus `"CONFIRMED"` akan ditolak oleh backend dengan error `HTTP 400 Bad Request` (`"Cannot edit time entity from a confirmed attendance period"`).
5. **Konversi Zona Waktu**  
   Server mengonversi string tanggal UTC ke zona waktu lokal (`convertUtcToLocal` menggunakan library `moment.utc(str).local()`). Jam kerja Indonesia Barat (WIB) berjarak UTC+7 (`08:00 WIB` = `01:00 UTC`, `17:00 WIB` = `10:00 UTC`).

---

## 4. Matriks Rute Endpoint

| Modul | Method | Endpoint Path | Autentikasi | Fungsi |
| :--- | :---: | :--- | :---: | :--- |
| **Auth** | `POST` | `/auth/verify` | Publik | Verifikasi & tukar token Auth0 ke access token Dotify |
| **Auth** | `POST` | `/auth/logout` | Bearer | Invalidation session / logout |
| **Tracker** | `GET` | `/tracker/check-status` | Bearer | Cek status stopwatch / timer aktif & user overtime |
| **Tracker** | `POST` | `/tracker/clock-in` | Bearer | Memulai stopwatch pekerjaan baru secara realtime |
| **Tracker** | `POST` | `/tracker/clock-out` | Bearer | Menghentikan stopwatch pekerjaan yang sedang berjalan |
| **Projects** | `GET` | `/projects` | Bearer | Mengambil daftar project (bisa dengan query `search`) |
| **Projects** | `GET` | `/projects/favorite` | Bearer | Mengambil daftar project favorit user |
| **Projects** | `POST` | `/projects/:id/favorite` | Bearer | Menandai project sebagai favorit (*star*) |
| **Projects** | `DELETE`| `/projects/:id/favorite` | Bearer | Menghapus status favorit project (*unstar*) |
| **Timesheet**| `GET` | `/time-entities` | Bearer | Mengambil histori log waktu per minggu |
| **Timesheet**| `GET` | `/time-entities/summary` | Bearer | Ringkasan akumulasi jam kerja minggu & periode |
| **Timesheet**| `GET` | `/time-entities/:id` | Bearer | Mengambil detail 1 log waktu spesifik |
| **Timesheet**| `PUT` | `/time-entities/:id` | Bearer | Mengubah project, deskripsi, atau jam selesai entri |
| **Timesheet**| `POST` | `/time-entities/bulk-create-by-dates` | Bearer | Membuat entri tanggal kosong (default 08:00 - 16:00) |
| **Timesheet**| `PATCH`| `/time-entities/bulk-update-to-normal-hour` | Bearer | Membatasi durasi kumulatif harian menjadi normal (8h) |
| **Timesheet**| `GET` | `/time-entities/get-all-date-empty` | Bearer | Mengambil daftar tanggal yang belum memiliki log |
| **Day Off** | `GET` | `/dayoff/periodical` | Bearer | Daftar hari libur nasional dan cuti yang disetujui |
| **System** | `GET` | `/health` | Publik | Health check status & uptime server |
| **System** | `POST` | `/clear-cache` | Publik | Membersihkan cache detail user di Redis |

---

## 5. Rincian Endpoint, Payload, & Contoh Respons

### 5.1. Authentication (`/auth`)

#### `POST /auth/verify`
Menukar access token dari Auth0 ke JWT internal Dotify sekaligus mengambil profil karyawan.

- **Request Body**:
```json
{
  "accessToken": "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6..."
}
```

- **Response (HTTP 200/201)**:
```json
{
  "message": "Verify auth0 successfully",
  "data": {
    "access_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "user_profile": {
      "id": 121,
      "name": "Ega Gofur",
      "email": "ega.gofur@dot.co.id",
      "role": "Software Engineer",
      "avatar": "https://lh3.googleusercontent.com/a/...",
      "clockify_email": "ega.gofur@dot.co.id",
      "one_signal_external_user_id": "0191c498-..."
    }
  }
}
```

#### `POST /auth/logout`
Logout dan invalidate token pengguna.

- **Request Body**: *None (Header `Authorization` wajib disertakan)*
- **Response (HTTP 200)**:
```json
{
  "message": "Logout successfully",
  "data": null
}
```

---

### 5.2. Tracker / Stopwatch (`/tracker`)

#### `GET /tracker/check-status`
Mengecek apakah ada timer/stopwatch yang sedang aktif dan apakah status user saat ini masuk kategori overtime.

- **Query Params**: *None*
- **Response (HTTP 200 - Saat Timer Aktif)**:
```json
{
  "message": "Check status successfully",
  "data": {
    "is_start_timer": true,
    "is_over_time": false,
    "time_entity": {
      "id": 191356,
      "description": "Task pengerjaan modul",
      "project": {
        "id": 222,
        "name": "DLTX Solutions"
      },
      "start_date": "2026-10-08T02:06:41.000Z",
      "end_date": null
    }
  }
}
```

- **Response (HTTP 200 - Saat Idle / Tidak Ada Timer Aktif)**:
```json
{
  "message": "Check status successfully",
  "data": {
    "is_start_timer": false,
    "is_over_time": false,
    "time_entity": null
  }
}
```

#### `POST /tracker/clock-in`
Memulai pencatatan stopwatch pekerjaan secara realtime.

- **Request Body**:
```json
{
  "projectId": 222,
  "description": "feat(engine1): pengerjaan kalkulasi prioritas repair"
}
```

- **Response (HTTP 200/201)**:
```json
{
  "message": "Start timer successfully",
  "data": {
    "id": 191357,
    "description": "feat(engine1): pengerjaan kalkulasi prioritas repair",
    "status": "NEED APPROVAL",
    "note": null,
    "duration": 0,
    "attendance_confirmation_id": null,
    "clockify_time_entity_id": "",
    "user_id": 121,
    "task_id": null,
    "overtime_note": null,
    "revised_duration": null,
    "start_date": "2026-10-08T03:15:00.000Z",
    "end_date": null,
    "project": {
      "id": 222,
      "name": "DLTX Solutions"
    }
  }
}
```

#### `POST /tracker/clock-out`
Menghentikan stopwatch aktif dan menghitung total durasi secara otomatis.

- **Request Body**: *None (kosong)*
- **Response (HTTP 200)**:
```json
{
  "message": "Stop timer successfully",
  "data": {
    "id": 191357,
    "description": "feat(engine1): pengerjaan kalkulasi prioritas repair",
    "status": "NEED APPROVAL",
    "note": null,
    "duration": 4.25,
    "attendance_confirmation_id": null,
    "clockify_time_entity_id": "",
    "user_id": 121,
    "task_id": null,
    "overtime_note": null,
    "revised_duration": null,
    "start_date": "2026-10-08T03:15:00.000Z",
    "end_date": "2026-10-08T07:30:00.000Z",
    "project": {
      "id": 222,
      "name": "DLTX Solutions"
    }
  }
}
```

---

### 5.3. Projects (`/projects`)

#### `GET /projects`
Mengambil daftar project. Mendukung pencarian via query `search`.

- **Query Params**:
  - `search` (opsional): kata kunci nama atau kode project (contoh: `?search=DLTX`)
- **Response (HTTP 200)**:
```json
{
  "message": "Get projects successfully",
  "data": [
    {
      "id": 222,
      "name": "DLTX Solutions"
    },
    {
      "id": 203,
      "name": "Resona Maintenance"
    },
    {
      "id": 74,
      "name": "DOT Event"
    }
  ]
}
```

#### `GET /projects/favorite`
Mengambil daftar project yang telah dibintangi (*favorited*) oleh pengguna.

- **Response (HTTP 200)**:
```json
{
  "message": "Get project favorites successfully",
  "data": [
    {
      "id": 222,
      "name": "DLTX Solutions"
    },
    {
      "id": 200,
      "name": "Dotify 2026"
    }
  ]
}
```

#### `POST /projects/:id/favorite`
Menambahkan project ke daftar favorit.

- **URL Params**: `:id` (Integer) ID project
- **Response (HTTP 200/201)**:
```json
{
  "message": "Favorite a project successfully",
  "data": [
    {
      "id": 222,
      "name": "DLTX Solutions"
    }
  ]
}
```

#### `DELETE /projects/:id/favorite`
Menghapus project dari daftar favorit.

- **URL Params**: `:id` (Integer) ID project
- **Response (HTTP 200)**:
```json
{
  "message": "Remove a favorite project successfully",
  "data": []
}
```

---

### 5.4. Timesheet & Historical Time Entities (`/time-entities`)

#### `GET /time-entities`
Mengambil riwayat log kerja karyawan (rentang 1 bulan terakhir) yang dikelompokkan berdasarkan minggu (`week`).

- **Query Params**: *None*
- **Response (HTTP 200)**:
```json
{
  "message": "Get time entitites successfully",
  "data": [
    {
      "week": "2026-10-05_2026-10-11",
      "time_entities": [
        {
          "id": 191256,
          "description": "feat(engine1): carryover keputusan perbaikan antar run",
          "status": "NEED APPROVAL",
          "note": null,
          "duration": 4.44,
          "attendance_confirmation_id": null,
          "clockify_time_entity_id": "",
          "user_id": 121,
          "task_id": null,
          "overtime_note": null,
          "revised_duration": null,
          "start_date": "2026-10-07T06:03:26.000Z",
          "end_date": "2026-10-07T10:30:00.000Z",
          "project": {
            "id": 222,
            "name": "DLTX Solutions"
          },
          "attendance_confirmation": null
        }
      ]
    }
  ]
}
```

#### `GET /time-entities/summary`
Mengambil statistik jam kerja minggu ini dan periode absensi berjalan (tanggal 21 bulan lalu s/d 20 bulan ini).

- **Response (HTTP 200)**:
```json
{
  "message": "Get time entitites successfully",
  "data": {
    "employee_week_hours": 42.62,
    "this_week_normal_hours": 40.0,
    "employee_period_hours": 124.2,
    "this_period_normal_hours": 176.0
  }
}
```

#### `GET /time-entities/:id`
Mengambil detail 1 log waktu berdasarkan ID.

- **URL Params**: `:id` (Integer)
- **Response (HTTP 200)**:
```json
{
  "message": "Get detail time entity successfully",
  "data": {
    "id": 191256,
    "description": "feat(engine1): carryover keputusan perbaikan",
    "status": "NEED APPROVAL",
    "note": null,
    "duration": 4.44,
    "attendance_confirmation_id": null,
    "clockify_time_entity_id": "",
    "user_id": 121,
    "task_id": null,
    "overtime_note": null,
    "revised_duration": null,
    "start_date": "2026-10-07T06:03:26.000Z",
    "end_date": "2026-10-07T10:30:00.000Z",
    "project": {
      "id": 222,
      "name": "DLTX Solutions"
    }
  }
}
```

#### `PUT /time-entities/:id`
Memperbarui project, deskripsi, atau jam selesai (`endDate`).  
*(Perhatian: `startDate` tidak bisa diedit via endpoint ini).*

- **URL Params**: `:id` (Integer) ID entitas
- **Request Body**:
```json
{
  "projectId": 222,
  "description": "feat(engine1): pembaruan deskripsi task yang akurat",
  "endDate": "2026-10-07T10:30:00.000Z"
}
```

- **Response (HTTP 200)**:
```json
{
  "message": "Update time entity successfully",
  "data": {
    "id": 191256,
    "description": "feat(engine1): pembaruan deskripsi task yang akurat",
    "status": "NEED APPROVAL",
    "duration": 4.44,
    "start_date": "2026-10-07T06:03:26.000Z",
    "end_date": "2026-10-07T10:30:00.000Z",
    "project": {
      "id": 222,
      "name": "DLTX Solutions"
    }
  }
}
```

#### `POST /time-entities/bulk-create-by-dates`
Membuat entri log waktu pada tanggal-tanggal kosong. Backend otomatis mengisi rentang jam `08:00 - 16:00` (8 jam).

- **Request Body**:
```json
{
  "dates": ["2026-10-08"],
  "projectId": 222,
  "description": "feat(auth): implementasi alur login dan refresh token"
}
```

- **Response (HTTP 200/201)**:
```json
{
  "message": "Create time entities by dates successfully",
  "data": [
    {
      "id": 191358,
      "description": "feat(auth): implementasi alur login dan refresh token",
      "status": "NEED APPROVAL",
      "duration": 8.0,
      "attendance_confirmation_id": null,
      "clockify_time_entity_id": " ",
      "user_id": 121,
      "start_date": "2026-10-08T01:00:00.000Z",
      "end_date": "2026-10-08T09:00:00.000Z",
      "project": {
        "id": 222,
        "name": "DLTX Solutions"
      }
    }
  ]
}
```

#### `PATCH /time-entities/bulk-update-to-normal-hour`
Membatasi akumulasi jam kerja harian menjadi maksimal 8 jam (standar jam normal Dotify). Log overtime yang statusnya `NEED APPROVAL` akan dipotong end date-nya agar durasi kumulatif hari tersebut tepat 8 jam.

- **Request Body**: *None (kosong)*
- **Response (HTTP 200)**:
```json
{
  "message": "Update all entity successfully",
  "data": null
}
```

#### `GET /time-entities/get-all-date-empty`
Mengambil array tanggal (format `YYYY-MM-DD`) pada bulan tertentu yang belum memiliki entri log waktu.

- **Query Params**:
  - `date`: Format `YYYY-MM` (contoh: `?date=2026-10`)
- **Response (HTTP 200)**:
```json
{
  "message": "Get all empty time entity dates successfully",
  "data": [
    "2026-10-09",
    "2026-10-10"
  ]
}
```

---

### 5.5. Day Off & Holidays (`/dayoff`)

#### `GET /dayoff/periodical`
Mengambil daftar hari libur resmi dan pengajuan cuti yang telah disetujui dalam periode berjalan.

- **Query Params**: *None*
- **Response (HTTP 200)**:
```json
{
  "message": "Get day off data successfully",
  "data": {
    "holidays": [
      {
        "id": 45,
        "name": "Tahun Baru Islam",
        "start_date": "2026-07-08T00:00:00.000Z",
        "end_date": "2026-07-08T23:59:59.000Z"
      }
    ],
    "leave_applications": [
      {
        "id": 12,
        "reason": "Cuti Tahunan",
        "start_date": "2026-09-29T00:00:00.000Z",
        "end_date": "2026-09-29T23:59:59.000Z",
        "status": "APPROVED"
      }
    ]
  }
}
```

---

### 5.6. System Diagnostics & Cache (`/health`, `/clear-cache`)

#### `GET /health`
Mengecek status kesehatan service API. Tidak memerlukan autentikasi Bearer.

- **Response (HTTP 200)**:
```json
{
  "uptime": 1745950.43,
  "message": "API OK",
  "timestamp": 1791473394570
}
```

#### `POST /clear-cache`
Membersihkan cache detail user di Redis backend.

- **Request Body**: *None*
- **Response (HTTP 200)**:
```json
{
  "message": "Cache success deleted",
  "data": ["users_detail"]
}
```

---

## 6. Contoh Pemanggilan (cURL & Python)

### 6.1. cURL: Cek Status Stopwatch
```bash
curl -s -X GET "https://api.dotify.dot.co.id/api/v1/tracker/check-status" \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Accept: application/json"
```

### 6.2. cURL: Update Deskripsi Log Waktu
```bash
curl -s -X PUT "https://api.dotify.dot.co.id/api/v1/time-entities/191256" \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{
    "projectId": 222,
    "description": "feat(engine1): integrasi smart stock dan memo perbaikan",
    "endDate": "2026-10-07T10:30:00.000Z"
  }'
```

### 6.3. Python: Helper Sederhana untuk Agent
```python
import json, os, subprocess

def call_dotify_api(method: str, path: str, body: dict = None):
    config_path = os.path.expanduser("~/.config/dotify/config.json")
    with open(config_path) as f:
        cfg = json.load(f)

    token = cfg.get("token", "")
    if not token.startswith("Bearer "):
        token = "Bearer " + token

    base_url = cfg.get("base_url", "https://api.dotify.dot.co.id/api/v1").rstrip("/")
    url = f"{base_url}/{path.lstrip('/')}"

    cmd = [
        "curl", "-s", "-X", method,
        "-H", f"Authorization: {token}",
        "-H", "Accept: application/json"
    ]
    if body is not None:
        cmd.extend(["-H", "Content-Type: application/json", "-d", json.dumps(body)])
    cmd.append(url)

    res = subprocess.run(cmd, capture_output=True, text=True)
    return json.loads(res.stdout)

# Contoh penggunaan:
status = call_dotify_api("GET", "/tracker/check-status")
print(status["data"]["is_start_timer"])
```
