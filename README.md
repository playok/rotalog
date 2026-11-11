# rotalog

Apache rotatelogs와 호환되는 로그 회전 도구를 Rust로 처음부터 다시 구현한
프로젝트입니다.

Apache httpd의 `support/rotatelogs.c`가 제공하는 **기능과 옵션 의미**를 기준으로,
Rust에 맞게 다시 설계했습니다.

기존 `httpd.conf`의 `CustomLog`/`ErrorLog` 파이프에 그대로 넣을 수 있도록 옵션 문자와
위치 인자 문법을 유지했습니다. 내부 구현은 C의 전역 상태·APR 풀·`exit()` 중심 흐름 대신
소유권 기반 상태 머신, 명시적 오류 전파, 스레드 안전한 백그라운드 유지보수로 바꿨습니다.

```apache
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 86400" combined
ErrorLog  "|/usr/local/bin/rotalog /var/log/httpd/error.log 100M"
```

## Rust로 다시 만들면서 달라진 점

- **덮어쓰기 사고 방지**: `%Y-%m-%d`처럼 입도가 거친 패턴에서 같은 구간에 크기 회전이
  여러 번 일어나면 Apache는 같은 파일을 덮어씁니다. 이 구현은 `.1`, `.2`를 붙여
  기존 로그를 보존합니다.
- **회전 실패 시 로그 유실 방지**: 새 파일을 열지 못하면 기존 파일에 계속 쓰면서 5초
  간격으로 재시도합니다. Apache는 기존 파일을 truncate하고 손실 메시지 수를 기록한 뒤
  계속 진행합니다.
- **원자적 링크 갱신**: `-L`은 임시 이름으로 링크를 만든 뒤 `rename`으로 교체합니다.
  `tail -F`가 링크 부재 구간을 보지 않습니다. 하드 링크가 불가능한 파일 시스템에서는
  심볼릭 링크로 자동 대체합니다.
- **비동기 gzip + 보존 정책**: `-z`와 `--retain`은 별도 유지보수 스레드에서 처리합니다.
  큰 로그를 압축하는 동안 httpd 파이프가 멈추지 않습니다.
- **우아한 종료**: `SIGTERM`/`SIGINT`를 받으면 이미 큐에 들어온 로그를 기록하고
  flush한 뒤 종료 코드 0으로 끝납니다.
- **안전한 동시성**: stdin 리더 스레드와 단일 writer 스레드가 64 KiB × 64 슬롯의
  bounded channel로 연결됩니다. 메모리 사용량이 상한을 가지며 파일 상태에는 락이
  필요 없습니다.
- **`unsafe` 금지**: 크레이트 전체에 `#![forbid(unsafe_code)]`가 적용되어 있습니다.
- **결정적 테스트**: 회전 로직이 “현재 시각”을 인자로 받도록 설계되어, sleep 없이
  시간·크기·DST 인접 경계를 단위 테스트할 수 있습니다.

## 빌드와 설치

```sh
cargo build --release
# 결과: target/release/rotalog

cargo install --path .
# 또는
install -m 0755 target/release/rotalog /usr/local/bin/rotalog

# 기존 httpd 설정이 rotatelogs라는 이름을 기대한다면 심볼릭 링크로 호환할 수 있습니다.
ln -s /usr/local/bin/rotalog /usr/local/bin/rotatelogs
```

의존성은 `chrono`(시간대/strftime), `flate2`(gzip), `signal-hook`(시그널)뿐입니다.

## 사용법

```text
rotalog [OPTIONS] <logfile> <rotationtime|filesize> [<filesize>] [<offset>]
```

- `<logfile>`: 출력 경로 또는 `strftime(3)` 패턴. `%`가 포함되면 패턴으로 해석합니다.
- `<rotationtime>`: 초 단위 회전 간격 (정수).
- `<filesize>`: 대문자 `B`, `K`, `M`, `G` 접미사가 붙은 최대 크기. 예: `512K`, `100M`, `2G`.
- `<offset>`: UTC 기준 분 단위 오프셋 (`-840`..`840`). 마지막 위치 인자로만 사용할 수 있습니다.
- 시간과 크기를 함께 지정할 수 있습니다: `3600 100M`.
- 옵션은 원본과 동일하게 **첫 번째 위치 인자(logfile) 앞**에 와야 합니다. `--`는
  옵션 처리를 끝냅니다. logfile 뒤의 인자는 옵션으로 해석되지 않습니다.

### 주요 옵션

| 옵션 | 설명 |
| --- | --- |
| `-l`, `--local-time` | 시스템 로컬 시간대를 기준으로 회전·파일명 계산 |
| `-L`, `--link <path>` | 현재 로그를 가리키는 링크를 원자적으로 갱신 |
| `-p`, `--program <path>` | 새 파일을 열 때마다 `path <새 파일> [<이전 파일>]` 실행 |
| `-f`, `--force` | 첫 데이터를 기다리지 않고 시작 즉시 파일 생성 |
| `-D`, `--create-dirs` | 로그 경로의 상위 디렉터리 자동 생성 (패턴 경로 포함) |
| `-t`, `--truncate` | 회전 대신 같은 경로를 truncate. 패턴의 `%` 확장은 유지 |
| `-T`, `--truncate-rotated` | 최초 파일을 제외하고, 열리는 회전 대상 파일을 truncate |
| `-e`, `--echo` | 로그를 stdout으로도 복사 |
| `-c`, `--create-empty` | 입력이 없어도 시간 구간마다 빈 파일 생성 |
| `-n`, `--circular <count>` | `base`, `base.1`, … `base.(count-1)` 순환 이름 사용 |
| `-v`, `--verbose` | 설정과 파일 열기/회전/링크 동작을 stderr에 출력 |
| `-h`, `--help` | 도움말 |
| `-V`, `--version` | 버전 |

### Apache rotatelogs에는 없는 확장

| 옵션 | 설명 |
| --- | --- |
| `-z`, `--compress` | 회전된 파일을 백그라운드에서 gzip 압축 (`*.gz`) |
| `--retain <count>` | 이 프로세스가 만든 회전 파일을 최신 `count`개만 남김 |

제약:

- `-z`는 `-t`, `-T`, `-n`과 함께 쓸 수 없습니다. 같은 이름을 재사용하거나 즉시
  truncate하는 모드에서는 압축 대상이 모호해지기 때문입니다.
- `--retain`은 `-n`, `-t`, `-T`와 함께 쓸 수 없습니다.
- `-c`는 시간 간격이 있을 때만 효과가 있습니다. 크기 전용 구성에서도 원본처럼
  옵션은 허용되지만 빈 파일을 만들지 않습니다. 첫 빈 파일은 시작 후 한 interval이
  지난 뒤부터 생성됩니다.
- `-l`과 `<offset>`은 동시에 지정할 수 없습니다.
- `-t`는 원본과 동일하게 `-T`/`-n`보다 우선합니다. 함께 지정하면 `-T`/`-n`은
  무시되고 한 파일을 truncate합니다.
- `-n`은 `strftime` 패턴과 함께 쓸 수 없습니다.
- 파일 크기 단위는 대문자만 인정합니다. 소문자 `b/k/m/g`는 원본에서 `atoi()` 때문에
  시간 값으로 해석될 수 있지만, 이 구현은 의미가 모호한 입력을 명확히 거부합니다.

추가 주의:

- `-p`와 `-z`/`--retain`을 함께 쓰면, 비동기로 실행된 회전 후 프로그램과 압축/삭제
  작업이 경쟁할 수 있습니다. 이전 파일을 읽는 스크립트는 파일이 이미 압축되었거나
  삭제되었을 가능성을 처리해야 합니다.
- `-p` 프로그램은 rotalog의 stdin/stdout/stderr를 그대로 상속합니다. 원본
  Apache 동작과 같으므로, 프로그램이 stdin에서 읽으면 로그 파이프를 소비할 수 있습니다.
- `strftime` 패턴의 시간 입도는 회전 간격보다 촘촘해야 합니다. 예를 들어 하루 간격으로
  회전하면서 `%Y`만 쓰면 같은 이름이 반복되어 `.1`, `.2` 접미사 파일이 생깁니다.
- `-n`으로 재사용되는 이전 파일이나 `-t`로 truncate되는 파일을 `-p` 스크립트가 읽는다면,
  스크립트가 실행되는 시점에는 내용이 이미 바뀌었을 수 있습니다. 필요한 내용은 즉시
  복사하도록 작성하세요.

## 파일 이름 규칙

1. 이름에 `%`가 있으면 `strftime` 패턴으로 확장합니다.
   `access-%Y%m%d.log` → `access-20261111.log`
2. `-n N`이면 `logfile`, `logfile.1`, …, `logfile.(N-1)`을 순환합니다.
3. `-t`에서 `%`가 없는 이름은 그대로 사용합니다.
4. 그 외에는 `logfile.<10자리 조정 유닉스 시각>`을 사용합니다.
   시각은 시간 회전 구간의 시작점에 맞춰 정렬됩니다.

예:

```sh
printf 'hello\n' | rotalog /var/log/access.log 86400
# /var/log/access.log.1794355200  (하루 경계로 정렬된 시각)
```

시간 구간은 Apache와 동일하게 “UTC + 오프셋”을 정수 나눗셈으로 내림 정렬합니다.
`-l`을 쓰면 실행 시점의 OS 시간대 오프셋을 사용하므로 DST 전환을 따라갑니다.

크기 제한 때문에 구간 중간에 회전하면, 새 파일 이름에는 구간 시작 시각이 아니라
**실제 회전 시각**이 들어갑니다. 이때 패턴이 같은 이름을 다시 만들면 `.1`, `.2`가
추가되어 기존 파일을 절대 덮어쓰지 않습니다.

```text
access.20261111.log
access.20261111.log.1
access.20261111.log.2
```

## 동작 방식

### 회전 판정

쓰기 직전에 다음 순서로 검사합니다.

1. 열린 파일이 없으면 새로 엽니다.
2. 시간 제한이 있고 `현재 보정 시각 >= 구간 종료`이면 시간 회전합니다.
3. 크기 제한이 있고 `현재 크기 + 이번 청크 > 제한`이면 크기 회전합니다.

따라서 일반적인 청크에서는 파일이 제한을 넘지 않습니다. 다만 한 번의 stdin 읽기가
제한보다 큰 경우에는 그 청크를 통째로 기록하므로 파일이 그만큼 커질 수 있습니다.
(테스트와 파이프 처리의 원자성을 위한 선택입니다.) Apache는 “이미 제한을 초과한 뒤
다음 쓰기에서 회전”하지만, 이 구현은 “다음 청크가 제한을 넘으면 쓰기 전에 회전”합니다.
둘 다 한 청크만큼의 오차를 허용하며, 이 구현이 실제 최대 크기에 더 가깝습니다.

### 회전 순서

1. 새 파일 이름 계산 (필요 시 충돌 접미사)
2. `-D`이면 상위 디렉터리 생성
3. 새 파일을 `O_APPEND` 또는 `O_TRUNC`로 열기
4. `-L` 링크를 원자적으로 갱신
5. `-p` 프로그램을 비동기로 실행
6. 이전 파일 핸들 close
7. `-z`/`--retain`이 설정되어 있으면 이전 경로를 유지보수 큐에 전달
8. 현재 파일 상태 교체

새 파일 열기에 실패하면 이전 파일을 그대로 유지하고 5초 후 재시도합니다. 링크나
`-p` 실행 실패는 진단 메시지만 남기고 로깅을 계속합니다.

`-c`는 원본과 동일하게 시작 즉시 현재 구간 파일을 만들지 않습니다. 시작 후 한
interval이 지난 시점부터 빈 파일을 만들고, 이후에는 각 구간 경계를 따릅니다.
`-f`가 함께 있으면 시작 즉시 파일을 연 뒤 구간마다 회전합니다.

### 스레드 모델

```text
httpd ──stdin──▶ reader thread ──bounded channel──▶ writer thread ──▶ logfile
                                                          │
                                                          ├──(-e)──▶ stdout
                                                          │
                                                          └──retired path──▶ maintenance thread
                                                                               ├─ gzip
                                                                               └─ retention delete
```

- 리더는 64 KiB씩 읽어 `sync_channel(64)`로 전달합니다. writer가 느리면 자연스럽게
  backpressure가 걸리고 메모리는 약 4 MiB로 제한됩니다.
- writer만 회전 상태와 파일 핸들을 소유하므로 락이 없습니다.
- maintenance 스레드는 압축과 삭제를 담당합니다. 종료 시 큐를 모두 비운 뒤 join합니다.
- `SIGTERM`/`SIGINT`는 `signal-hook` 플래그로 전달됩니다. writer는 최대 200 ms 안에
  이를 확인하고, 큐에 남은 데이터를 기록한 뒤 정상 종료합니다.

## Apache httpd 설정 예

```apache
# 하루 단위, UTC 기준
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 86400" combined

# 로컬 시간 기준, 날짜 파일명
CustomLog "|/usr/local/bin/rotalog -l /var/log/httpd/access.%Y%m%d.log 86400" combined

# 시간 + 크기 동시 제한
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 3600 100M" combined

# 현재 로그 링크 + tail -F
CustomLog "|/usr/local/bin/rotalog -L /var/log/httpd/access-current.log /var/log/httpd/access.log 86400" combined

# 7개 순환 파일
CustomLog "|/usr/local/bin/rotalog -n 7 /var/log/httpd/access.log 100M" combined

# gzip + 최근 14개 보관 (확장)
CustomLog "|/usr/local/bin/rotalog -z --retain 14 /var/log/httpd/access.%Y%m%d.log 100M" combined
```

`-p`는 셸을 거치지 않고 직접 실행됩니다. 인자가 필요한 경우 래퍼 스크립트를 사용하세요.

```sh
#!/bin/sh
exec /usr/local/bin/post-rotate --tag httpd "$@"
```

## 원본 CLI 호환성 검증

`tests/original_cli_compat.rs`는 실제 Apache `rotatelogs` 바이너리를 oracle로 삼아
같은 argv를 양쪽에 실행하고 결과를 비교합니다.

- oracle 탐색 순서: `ROTATELOGS_ORACLE` 환경 변수 → `/usr/sbin/rotatelogs` →
  `/usr/local/sbin/rotatelogs` → Homebrew httpd 경로 → `PATH`의 `rotatelogs`.
- oracle이 없으면 테스트는 skip 메시지를 출력하고 성공합니다. 따라서 Apache가 없는
  CI에서도 `cargo test`가 깨지지 않습니다.
- 검증 규모: 공통 argv 매트릭스 51개, malformed 입력 8개, 확장 옵션 5개,
  `-n`/`-t`/`-L`/`-e` 동작 비교 3개.
- 비교 항목:
  - 명령 수용/거부 여부와 종료 코드
  - `-v`가 출력하는 유효 설정(회전 시간·크기·오프셋·로컬 시간·`strftime`·`-f`·
    `-D`·`-c`·`-p`·logfile)
  - `-n`, `-t`, `-L`, `-e`의 실제 파일/표준출력 동작
- 검증하는 유효 형식:
  - 시간만, 크기만, 시간+크기(양쪽 순서), 마지막 UTC 오프셋
  - `-l`, `-f`, `-D`, `-e`, `-t`, `-T`, `-n`, `-L`, `-p`, `-c`, `-v`
  - `-c`는 시간 간격에서 빈 파일을 만들고, 크기 전용에서는 원본처럼 허용되지만 무효
  - 붙여 쓴 값(`-n3`, `-Lpath`), 결합 플래그(`-lveD`), `--`
  - `strftime` 패턴, `-t`와 `-T`/`-n`의 우선순위
- 거부 형식: 인자 부족/과다, 시간·크기 중복, 0 값, `-l`+오프셋, `-n` 0/음수/
  비숫자, 패턴+`-n`, 알 수 없는 단축 옵션, 값 누락.

실행:

```sh
# 자동 탐색
cargo test --test original_cli_compat -- --nocapture

# 특정 oracle 지정
ROTATELOGS_ORACLE=/usr/sbin/rotatelogs \
  cargo test --test original_cli_compat -- --nocapture
```

### 의도적으로 다른 부분

원본은 `atoi()`와 바이트 스캐너를 사용해 잘못된 입력을 조용히 변환합니다. 이 구현은
같은 입력을 명확한 오류로 거부합니다.

| 입력 | 원본 | rotalog |
| --- | --- | --- |
| `5m` | 시간 5초 (`atoi`) | 거부 (단위는 대문자) |
| `5MB` | 크기 5바이트 (`B`를 먼저 찾음) | 거부 |
| `60abc` | 시간 60초 | 거부 |
| `60 abc` (마지막 인자) | 오프셋 0 | 거부 |
| `60 99999` | 오프셋 99999분 | 거부 (범위 초과) |
| `-n 3x` | `-n 3` | 거부 |
| `--anything` | 조용히 무시 | 거부 (알 수 없는 long option) |

다음은 원본에 없는 확장입니다: `-h`, `-V`, 모든 long option, `-z`, `--retain`.

## 테스트

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- 단위 테스트 59개: CLI 파싱, 시간 정렬, strftime 검증, 이름 충돌, `-n`/`-t`/`-T`,
  링크, gzip, 보존 정책.
- 통합 테스트 19개: 실제 바이너리에 stdin을 공급해 파일 생성, 크기/시간 회전,
  `-e`, `-L`, `-p`, `-D`, `-f`, `-c`, `-z`/`--retain`, 잘못된 설정, SIGTERM 우아한 종료.
- 원본 CLI 호환성 테스트 4개: 실제 Apache `rotatelogs`와 설정·종료 코드·파일 동작 비교.

## Apache 원본과의 차이 및 제한

- 옵션 문자, 위치 인자 순서, 시간 정렬, `-n`/`-t`/`-T`/`-c`/`-p`의 의미는 원본을
  따릅니다. `-t`가 `-T`/`-n`보다 우선하는 동작도 같습니다.
- POSIX getopt와 동일하게 옵션은 logfile 앞에서만 해석되며, `--`가 옵션 처리를
  끝냅니다.
- 잘못된 숫자, 소문자 크기 단위, 범위를 벗어난 UTC 오프셋, 알 수 없는 long option은
  원본의 `atoi()`/무시 동작 대신 명확한 오류로 거부합니다.
- 4 GiB를 넘는 `G` 크기와 큰 시간/카운트는 64비트로 정확히 계산합니다. 원본은
  32비트 `unsigned int`/`atoi()` 오버플로가 발생할 수 있습니다.
- `-h`, `-V`, long option, `-z`, `--retain`은 원본에 없는 확장입니다.
- 같은 이름 충돌 시 `.1`, `.2`를 붙여 보존합니다 (원본은 덮어쓸 수 있음).
- 회전 실패 시 이전 파일에 계속 기록합니다 (원본은 truncate 후 손실 메시지 기록).
- `-L`은 하드 링크를 우선하되, 불가능하면 심볼릭 링크로 대체합니다.
- `--retain`은 **현재 프로세스가 회전시킨 파일만** 추적합니다. 재시작 이전에 만들어진
  오래된 아카이브는 자동으로 정리하지 않습니다. 장기 보존 정책은 logrotate 같은
  외부 도구와 함께 쓰는 것을 권장합니다.
- `strftime` 패턴은 UTF-8이어야 합니다. `%`가 없는 경로는 비 UTF-8도 사용할 수 있습니다.
- `strftime`의 월/요일/`%c`/`%x`/`%X` 이름은 시스템 locale이 아니라 English 고정
  형식으로 출력됩니다.
- Unix 계열(httpd 파이프, `tail -F`, 하드 링크)을 주 대상으로 합니다.
- `-p`는 셸을 사용하지 않습니다. 파이프나 여러 인자가 필요하면 래퍼 스크립트를 쓰세요.
- `-e`의 stdout 쓰기 오류(EPIPE 포함)는 에코만 비활성화하고 파일 기록은 계속합니다.
- 상대 경로는 프로세스의 현재 작업 디렉터리(httpd가 실행한 경우 `ServerRoot`) 기준이며,
  로그 경로 자체의 심볼릭 링크는 정규화하지 않습니다.

## 종료 코드

| 코드 | 의미 |
| --- | --- |
| 0 | 정상 종료 (stdin EOF 또는 SIGTERM/SIGINT) |
| 1 | 잘못된 명령줄/설정 |
| 2 | 런타임 또는 I/O 오류 |

## 라이선스

MIT License. 자세한 내용은 `LICENSE`를 확인하세요.

동작과 CLI는 Apache httpd의 `support/rotatelogs.c`를 참고했지만, 원본 C 코드를
포함하지 않은 독립 구현입니다. 자세한 감사는 `CREDITS.md`에 있습니다.
