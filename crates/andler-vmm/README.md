# andler-vmm

**Пустой каркас.** Backend-заготовка под `rust-vmm` на будущее — не используется
до решения открытого вопроса "когда rust-vmm считается достаточно зрелой"
(см. `docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §10).

## Текущее состояние

`impl HypervisorBackend for VmmBackend` присутствует и регистрируется в
`andler-daemon`, но каждый метод возвращает
`Err(BackendError::NotImplemented { backend: "vmm", operation: ... })`.
Не `todo!()`/`unimplemented!()` — демон не должен паниковать, если пользователь
явно укажет этот backend до его готовности. См. §9 архитектурного плана, пункт 3.

Не добавляйте сюда реальную логику, пока не принято отдельное решение о старте
этого этапа.
