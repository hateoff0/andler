# andler-cli

Бинарник `andler` — тонкий CLI-клиент, который только формирует gRPC-запросы
через `andler-rpc` и форматирует ответ. Никакой бизнес-логики здесь быть не
должно — она целиком в `andler-daemon`/`andler-core`.

## Команды (план)

`create`, `start`, `stop`, `pause`, `resume`, `status`, `list`, `snapshot` —
по одной на каждый gRPC-метод из §5.1 архитектурного плана. См. `src/commands/`.
