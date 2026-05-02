//! 把 `BlockEntity` / `ClientboundBlockEntityData` 里的 `simdnbt::owned::Nbt`
//! 解码成下游能直接 query 的类型化字段。
//!
//! azalea 协议层只把 `Vec<BlockEntity>` 透传给客户端，`data: Nbt` 留作 raw NBT。
//! 渲染 / UI / bot 想读 chest items / sign text / banner pattern stack /
//! skull profile 这些字段时都得自己 walk NBT，每个下游各做一遍既慢又容易写错。
//! 本模块给出统一的"协议层 NBT → typed `BlockEntityData`"解码——目前只覆盖
//! vanilla 26.1 单机基线最显眼的几类（chest / sign / banner / skull / bell），
//! 其它 kind 走 [`BlockEntityData::Unknown`] 占位。
//!
//! ## 范围
//!
//! - **Chest / TrappedChest**：items 简化为 `(slot, id, count)` 三元组（完整
//!   `ItemStack` 解码涉及 `DataComponentPatch`，规模大、和 `azalea-buf` 协议读
//!   是同一份代码，留作 follow-up）；`CustomName` / `Lock` 标准字段。
//! - **Sign / HangingSign**：`front_text` / `back_text` 各 4 行 `FormattedText`
//!   + dyecolor + glowing；`is_waxed`。1.20+ 双面文字 schema 已是 vanilla 26.1。
//! - **Banner**：`patterns: List<{pattern: registry id, color: dye name}>` +
//!   `CustomName`。
//! - **Skull**：profile name / uuid / texture URL（从 `properties[name=textures]`
//!   的 base64-JSON 解出 `textures.SKIN.url`）+ `note_block_sound`。
//! - **Bell**：仅 `CustomName`（铃响动画走 `BlockEvent` 包，不在 NBT）。
//! - **Lectern**：`Book` 单 ItemStack（id + count，components 同样略）+ `Page`
//!   (i32) + `HasBook` (bool)。
//! - **BrewingStand**：3 槽 `Items`（同 chest item 结构，slot 0..2）+ `BrewTime`
//!   (i16) + `Fuel` (i8) + `CustomName`。
//! - **EndPortal**：vanilla `saveAdditional` 不写任何字段，但 BE 仍然存在——
//!   保留空 variant 让 visual 占位能 spawn。
//! - **ShulkerBox**：27 槽 `Items` + 可选 `Color` (byte 0..15 → DyeColor) +
//!   `CustomName`。注意 vanilla server 默认不把 color 写进 NBT（颜色由 block id
//!   编码），field 不在时 `color` 为 `None`。
//! - **其它 30+ kind**：[`BlockEntityData::Unknown`]，下游自己决定是否 walk
//!   原 NBT。
//!
//! ## 解码路径
//!
//! 协议层给的是 [`simdnbt::owned::Nbt`]，[`simdnbt::Deserialize`] 走的是 borrow
//! 形态——做一次 owned → bytes → borrow round-trip 转换，再走 `Deserialize::
//! from_compound`。owned/borrow 互转是 simdnbt 设计内的常规操作，cost 可忽略
//! （每个 BlockEntity 只 round-trip 一次，BlockEntity 整片 chunk 量级几十个）。

use azalea_chat::FormattedText;
use azalea_registry::{builtin::BlockEntityKind, identifier::Identifier};
use simdnbt::{Deserialize, FromNbtTag};
use tracing::{trace, warn};

/// 类型化 BlockEntity 数据。每个 variant 字段都是从原 NBT 解出的"vanilla 客户端
/// 渲染需要的最小集"。不识别 / 解码失败的走 [`Self::Unknown`]——下游想 fallback
/// 自己读原 NBT 的话仍能从协议字段拿到 [`simdnbt::owned::Nbt`]。
#[derive(Clone, Debug, PartialEq)]
pub enum BlockEntityData {
    Chest(ChestData),
    TrappedChest(ChestData),
    /// EnderChest 的 items 是 player-side（玩家自身的 inventory），block entity
    /// NBT 仅含 `CustomName` / `Lock`。
    EnderChest(ChestData),
    Sign(SignData),
    HangingSign(SignData),
    Banner(BannerData),
    Skull(SkullData),
    Bell(BellData),
    Lectern(LecternData),
    BrewingStand(BrewingStandData),
    /// vanilla `saveAdditional` 不写任何字段；保留 marker variant 让 visual /
    /// 粒子占位下游仍能区分"end_portal BE 在"和"无 BE"。
    EndPortal,
    ShulkerBox(ShulkerBoxData),
    /// kind 未在解码表里，或 NBT 解码失败。`kind` 仍给下游让它知道是个什么。
    Unknown { kind: BlockEntityKind },
}

impl BlockEntityData {
    /// 协议层 `BlockEntity::data` / `ClientboundBlockEntityData::tag` 都是
    /// [`simdnbt::owned::Nbt`]——这里做 owned→borrow round-trip 后调用每个
    /// kind 的 [`Deserialize::from_compound`]。失败时返回 `Unknown { kind }`，
    /// 不 panic、不丢 kind 信息。
    pub fn from_nbt(kind: BlockEntityKind, nbt: &simdnbt::owned::Nbt) -> Self {
        let simdnbt::owned::Nbt::Some(base) = nbt else {
            return Self::decode_empty(kind);
        };

        // owned → borrow：用 `write_unnamed` 写 COMPOUND_ID + body，然后
        // `borrow::read_unnamed` 解回。simdnbt 的 borrow form 持有 input 切片
        // 引用，bytes 必须在 borrow_nbt 用完前活着——下面整个 match 都在同一
        // 帧栈里，bytes 被 drop 之后 borrow_nbt 也已经 drop 完。
        let mut bytes = Vec::with_capacity(128);
        base.write_unnamed(&mut bytes);
        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let borrow_nbt = match simdnbt::borrow::read_unnamed(&mut cursor) {
            Ok(n) => n,
            Err(e) => {
                warn!("BlockEntityData::from_nbt: round-trip read failed for {kind:?}: {e:?}");
                return Self::Unknown { kind };
            }
        };
        let simdnbt::borrow::Nbt::Some(borrow_base) = borrow_nbt else {
            return Self::decode_empty(kind);
        };
        let compound = borrow_base.as_compound();

        macro_rules! decode {
            ($ty:ty, $variant:ident) => {
                match <$ty as Deserialize>::from_compound(compound) {
                    Ok(d) => Self::$variant(d),
                    Err(e) => {
                        trace!(
                            "BlockEntityData::from_nbt: {} decode failed for {:?}: {:?}",
                            stringify!($ty),
                            kind,
                            e
                        );
                        Self::Unknown { kind }
                    }
                }
            };
        }

        match kind {
            BlockEntityKind::Chest => decode!(ChestData, Chest),
            BlockEntityKind::TrappedChest => decode!(ChestData, TrappedChest),
            BlockEntityKind::EnderChest => decode!(ChestData, EnderChest),
            BlockEntityKind::Sign => decode!(SignData, Sign),
            BlockEntityKind::HangingSign => decode!(SignData, HangingSign),
            BlockEntityKind::Banner => decode!(BannerData, Banner),
            BlockEntityKind::Skull => decode!(SkullData, Skull),
            BlockEntityKind::Bell => decode!(BellData, Bell),
            BlockEntityKind::Lectern => decode!(LecternData, Lectern),
            BlockEntityKind::BrewingStand => decode!(BrewingStandData, BrewingStand),
            // EndPortal NBT 永远是空 compound，没有要解的字段——直接给 marker。
            BlockEntityKind::EndPortal => Self::EndPortal,
            BlockEntityKind::ShulkerBox => decode!(ShulkerBoxData, ShulkerBox),
            _ => Self::Unknown { kind },
        }
    }

    /// `Nbt::None`（vanilla 偶尔在没附加字段的 block entity 上发空 NBT）也按
    /// kind 给一个空 default `BlockEntityData`，让下游"BlockEntity 存在但没
    /// payload"和"BlockEntity 不存在 / 解码失败"两种情况能区分。
    fn decode_empty(kind: BlockEntityKind) -> Self {
        match kind {
            BlockEntityKind::Chest => Self::Chest(ChestData::default()),
            BlockEntityKind::TrappedChest => Self::TrappedChest(ChestData::default()),
            BlockEntityKind::EnderChest => Self::EnderChest(ChestData::default()),
            BlockEntityKind::Sign => Self::Sign(SignData::default()),
            BlockEntityKind::HangingSign => Self::HangingSign(SignData::default()),
            BlockEntityKind::Banner => Self::Banner(BannerData::default()),
            BlockEntityKind::Skull => Self::Skull(SkullData::default()),
            BlockEntityKind::Bell => Self::Bell(BellData::default()),
            BlockEntityKind::Lectern => Self::Lectern(LecternData::default()),
            BlockEntityKind::BrewingStand => Self::BrewingStand(BrewingStandData::default()),
            BlockEntityKind::EndPortal => Self::EndPortal,
            BlockEntityKind::ShulkerBox => Self::ShulkerBox(ShulkerBoxData::default()),
            _ => Self::Unknown { kind },
        }
    }

    /// 反查 kind（用作 `BlockEntityData → BlockEntityKind` 镜像，方便下游统计/
    /// 路由）。
    pub fn kind(&self) -> BlockEntityKind {
        match self {
            Self::Chest(_) => BlockEntityKind::Chest,
            Self::TrappedChest(_) => BlockEntityKind::TrappedChest,
            Self::EnderChest(_) => BlockEntityKind::EnderChest,
            Self::Sign(_) => BlockEntityKind::Sign,
            Self::HangingSign(_) => BlockEntityKind::HangingSign,
            Self::Banner(_) => BlockEntityKind::Banner,
            Self::Skull(_) => BlockEntityKind::Skull,
            Self::Bell(_) => BlockEntityKind::Bell,
            Self::Lectern(_) => BlockEntityKind::Lectern,
            Self::BrewingStand(_) => BlockEntityKind::BrewingStand,
            Self::EndPortal => BlockEntityKind::EndPortal,
            Self::ShulkerBox(_) => BlockEntityKind::ShulkerBox,
            Self::Unknown { kind } => *kind,
        }
    }
}

// ---- Chest ----------------------------------------------------------------

/// Chest 的 vanilla NBT schema：
/// ```text
/// {
///   "Items": [{ "Slot": byte, "id": "minecraft:stone", "count": int, "components"?: ... }, ...],
///   "CustomName"?: chat component,
///   "Lock"?: string | component,
/// }
/// ```
/// `components` 字段（item DataComponentPatch）规模太大，先丢——下游想用
/// vanilla item 比对时按 (id, count) 做即可。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ChestData {
    #[simdnbt(rename = "Items")]
    pub items: Option<Vec<ChestItem>>,
    #[simdnbt(rename = "CustomName")]
    pub custom_name: Option<FormattedText>,
    #[simdnbt(rename = "Lock")]
    pub lock: Option<FormattedText>,
}

/// chest 单格槽位记录。`slot` 是格内索引（0..27 对 single chest，0..54 对 double）。
/// `id` 用 [`Identifier`] 让下游直接拿 namespace/path；`count` 用 i32（vanilla
/// 1.20.5+ items count 字段升 int）。
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ChestItem {
    #[simdnbt(rename = "Slot")]
    pub slot: u8,
    pub id: Identifier,
    pub count: i32,
}

// ---- Sign -----------------------------------------------------------------

/// 1.20+ 双面 sign schema。client 渲染时按"面 + line"取 `messages[i]` 渲染。
/// `is_waxed` 决定 client 是否禁用编辑 GUI（与渲染无关，但 BRP 守线要看）。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SignData {
    pub front_text: Option<SignFace>,
    pub back_text: Option<SignFace>,
    pub is_waxed: Option<bool>,
}

/// 单面文字。messages 长度恒为 4（vanilla 写死），decode 后短于 4 的尾部 fill
/// 空 component；`color` 是 dyecolor name（"black" / "white" / ...）；
/// `has_glowing_text` 为荧光墨水效果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SignFace {
    pub messages: [FormattedText; 4],
    pub color: String,
    pub has_glowing_text: bool,
}

impl SignFace {
    fn empty_messages() -> [FormattedText; 4] {
        std::array::from_fn(|_| FormattedText::from(String::new()))
    }
}

impl Deserialize for SignFace {
    fn from_compound(
        compound: simdnbt::borrow::NbtCompound,
    ) -> Result<Self, simdnbt::DeserializeError> {
        let mut messages = Self::empty_messages();
        if let Some(list_tag) = compound.list("messages") {
            if let Some(items) = list_tag.compounds() {
                // 1.20.4+ schema：messages 是 4 条 chat-component compound。
                for (i, msg_compound) in items.into_iter().enumerate().take(4) {
                    if let Some(text) = FormattedText::from_nbt_compound(msg_compound) {
                        messages[i] = text;
                    }
                }
            } else if let Some(strings) = list_tag.strings() {
                // 1.20.0 schema：messages 是 4 条 JSON string。`FormattedText::from`
                // 接 `&Mutf8Str`，把 JSON 解成 component；非 JSON / 解析失败则视为
                // 纯文本。
                for (i, s) in strings.iter().enumerate().take(4) {
                    messages[i] = FormattedText::from(*s);
                }
            }
        }

        let color = compound
            .string("color")
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "black".to_owned());
        let has_glowing_text = compound
            .byte("has_glowing_text")
            .map(|b| b != 0)
            .unwrap_or(false);

        Ok(Self {
            messages,
            color,
            has_glowing_text,
        })
    }
}

// ---- Banner ---------------------------------------------------------------

/// 1.20.5+ banner schema：
/// ```text
/// {
///   "patterns": [{ "pattern": "minecraft:square_bottom_left", "color": "white" }, ...],
///   "CustomName"?: chat component,
/// }
/// ```
/// `pattern` 是 banner pattern registry id；`color` 是 dyecolor name。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct BannerData {
    pub patterns: Option<Vec<BannerPatternLayer>>,
    #[simdnbt(rename = "CustomName")]
    pub custom_name: Option<FormattedText>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct BannerPatternLayer {
    pub pattern: Identifier,
    pub color: String,
}

// ---- Skull ----------------------------------------------------------------

/// player_head / 各种 skull 共用 schema（1.20.5+）：
/// ```text
/// {
///   "profile"?: { "name"?: string, "id"?: int_array[4], "properties"?: [{name, value, signature?}] },
///   "note_block_sound"?: identifier,
/// }
/// ```
/// 1.20.4-：`SkullOwner: { Id, Name, Properties }` legacy schema 已不进 wire
/// （server 转过新 schema）。`texture_url` 由 [`SkullData::from_compound`] 在
/// decode `properties` 时尝试展开 base64+json，失败保留 `None`，不 panic。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkullData {
    pub profile_name: Option<String>,
    /// 4 个 i32 拼成的 UUID，按 vanilla `UUID.fromIntArray` 顺序：高位先。
    pub profile_uuid: Option<u128>,
    pub texture_url: Option<String>,
    pub note_block_sound: Option<Identifier>,
}

impl Deserialize for SkullData {
    fn from_compound(
        compound: simdnbt::borrow::NbtCompound,
    ) -> Result<Self, simdnbt::DeserializeError> {
        let mut data = SkullData::default();

        data.note_block_sound = compound
            .get("note_block_sound")
            .and_then(Identifier::from_nbt_tag);

        if let Some(profile) = compound.compound("profile") {
            data.profile_name = profile
                .string("name")
                .map(|s| s.to_string_lossy().into_owned());

            if let Some(arr) = profile.int_array("id")
                && arr.len() == 4
            {
                let hi = ((arr[0] as u32 as u64) << 32) | (arr[1] as u32 as u64);
                let lo = ((arr[2] as u32 as u64) << 32) | (arr[3] as u32 as u64);
                data.profile_uuid = Some(((hi as u128) << 64) | lo as u128);
            }

            if let Some(props) = profile.list("properties")
                && let Some(prop_compounds) = props.compounds()
            {
                for prop in prop_compounds {
                    let Some(name) = prop.string("name") else {
                        continue;
                    };
                    if name.to_string_lossy() != "textures" {
                        continue;
                    }
                    let Some(value) = prop.string("value") else {
                        continue;
                    };
                    data.texture_url = parse_skull_texture_value(&value.to_string_lossy());
                    break;
                }
            }
        } else if let Some(owner) = compound.compound("SkullOwner") {
            // legacy 1.16~1.20.4 schema fallback。新 server 不会发，但用户带的
            // 老世界 chunk 仍可能进 wire。
            data.profile_name = owner
                .string("Name")
                .map(|s| s.to_string_lossy().into_owned());
            if let Some(arr) = owner.int_array("Id")
                && arr.len() == 4
            {
                let hi = ((arr[0] as u32 as u64) << 32) | (arr[1] as u32 as u64);
                let lo = ((arr[2] as u32 as u64) << 32) | (arr[3] as u32 as u64);
                data.profile_uuid = Some(((hi as u128) << 64) | lo as u128);
            }
            if let Some(props) = owner.compound("Properties")
                && let Some(textures_list) = props.list("textures")
                && let Some(textures_compounds) = textures_list.compounds()
                && let Some(first) = textures_compounds.into_iter().next()
                && let Some(value) = first.string("Value")
            {
                data.texture_url = parse_skull_texture_value(&value.to_string_lossy());
            }
        }

        Ok(data)
    }
}

/// `value` 是 base64(JSON {"timestamp":..., "profileId":..., "profileName":...,
/// "textures": {"SKIN": {"url": "https://textures.minecraft.net/texture/..."}}})。
/// 我们手写一个最小提取器：只看第一个 `"url":"..."` 引号串——避免引入 base64
/// + serde_json 大依赖。失败安全，返回 `None`。
fn parse_skull_texture_value(b64: &str) -> Option<String> {
    let decoded = base64_decode(b64.as_bytes())?;
    let s = std::str::from_utf8(&decoded).ok()?;
    let key = "\"url\"";
    let key_pos = s.find(key)?;
    let after_key = &s[key_pos + key.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = &after_key[colon_pos + 1..];
    let quote_start = after_colon.find('"')?;
    let after_open = &after_colon[quote_start + 1..];
    let quote_end = after_open.find('"')?;
    Some(after_open[..quote_end].to_owned())
}

/// 极简 base64 解码：标准 alphabet + `=` padding；无空白容忍、无 URL-safe
/// 变体——skull texture value 是 vanilla 服务器写出的标准 base64。
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let trimmed = match input {
        [rest @ .., b'=', b'='] => rest,
        [rest @ .., b'='] => rest,
        all => all,
    };

    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in trimmed {
        let v = val(b)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---- Bell -----------------------------------------------------------------

/// vanilla bell 只在 NBT 写 `CustomName`；ringing 状态 server 用 `BlockEvent`
/// 包驱动，不在 BlockEntity NBT 里。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct BellData {
    #[simdnbt(rename = "CustomName")]
    pub custom_name: Option<FormattedText>,
}

// ---- Lectern --------------------------------------------------------------

/// Lectern 的 vanilla NBT schema（1.20+）：
/// ```text
/// {
///   "Book"?: { "id": "minecraft:written_book", "count": int, "components"?: ... },
///   "Page"?: int,
///   "HasBook"?: byte,
/// }
/// ```
/// `Book` 缺失视为没书；`Page` / `HasBook` 缺失分别给 0 / false 默认值——
/// vanilla 客户端渲染时也是这样兜底。`Book.components` 的书页文本规模大，
/// 与 chest 同样先丢，下游想读 `written_book_content` 走 component 系统。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct LecternData {
    #[simdnbt(rename = "Book")]
    pub book: Option<ChestItem>,
    #[simdnbt(rename = "Page")]
    pub page: Option<i32>,
    #[simdnbt(rename = "HasBook")]
    pub has_book: Option<bool>,
}

// ---- BrewingStand ---------------------------------------------------------

/// BrewingStand 的 vanilla NBT schema：
/// ```text
/// {
///   "Items": [{ "Slot": byte (0..=2 → 3 瓶 + 1 燃料 + 1 配料), "id": ..., "count": int }, ...],
///   "BrewTime"?: short,
///   "Fuel"?: byte,
///   "CustomName"?: chat component,
/// }
/// ```
/// 槽位含义按 vanilla：0..2 = 三瓶位、3 = 配料、4 = blaze powder。
/// `BrewTime` (i16) 是当前酿造剩余 tick；`Fuel` (i8) 是 blaze powder 燃料计数
/// （0..20）。两者缺失时按 vanilla 默认 0。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct BrewingStandData {
    #[simdnbt(rename = "Items")]
    pub items: Option<Vec<ChestItem>>,
    #[simdnbt(rename = "BrewTime")]
    pub brew_time: Option<i16>,
    #[simdnbt(rename = "Fuel")]
    pub fuel: Option<i8>,
    #[simdnbt(rename = "CustomName")]
    pub custom_name: Option<FormattedText>,
}

// ---- ShulkerBox -----------------------------------------------------------

/// ShulkerBox 的 vanilla NBT schema：
/// ```text
/// {
///   "Items": [{ "Slot": byte (0..27), "id": ..., "count": int }, ...],
///   "CustomName"?: chat component,
///   "Lock"?: string | component,
/// }
/// ```
/// 颜色不在 NBT 里——vanilla 用 17 种 block id 编码（`white_shulker_box` … +
/// 无色 `shulker_box`）。这里仍保留可选 `color` 字段：部分 datapack / mod 会
/// 把 dye color 写进 BE NBT 的 `Color` byte（0..15）；缺失时 `color = None`，
/// 渲染端按 block id 决定颜色即可。
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ShulkerBoxData {
    #[simdnbt(rename = "Items")]
    pub items: Option<Vec<ChestItem>>,
    #[simdnbt(rename = "CustomName")]
    pub custom_name: Option<FormattedText>,
    #[simdnbt(rename = "Lock")]
    pub lock: Option<FormattedText>,
    /// 0..15 dye index（`DyeColor as u8` 排序：white=0、orange=1、… black=15）。
    /// vanilla server 一般不写，留 None；下游想要 `DyeColor` 自己 cast。
    #[simdnbt(rename = "Color")]
    pub color: Option<u8>,
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use simdnbt::owned::{BaseNbt, Nbt, NbtCompound, NbtList, NbtTag};

    use super::*;

    fn wrap(compound: NbtCompound) -> Nbt {
        Nbt::Some(BaseNbt::new("", compound))
    }

    #[test]
    fn decode_chest_items_and_lock() {
        let mut item0 = NbtCompound::new();
        item0.insert("Slot", NbtTag::Byte(0));
        item0.insert("id", NbtTag::String("minecraft:stone".into()));
        item0.insert("count", NbtTag::Int(64));

        let mut item1 = NbtCompound::new();
        item1.insert("Slot", NbtTag::Byte(13));
        item1.insert("id", NbtTag::String("minecraft:diamond".into()));
        item1.insert("count", NbtTag::Int(3));

        let mut compound = NbtCompound::new();
        compound.insert("Items", NbtTag::List(NbtList::Compound(vec![item0, item1])));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Chest, &nbt);
        let BlockEntityData::Chest(chest) = decoded else {
            panic!("expected Chest variant, got {decoded:?}");
        };
        let items = chest.items.expect("Items decoded");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].slot, 0);
        assert_eq!(items[0].id.to_string(), "minecraft:stone");
        assert_eq!(items[0].count, 64);
        assert_eq!(items[1].slot, 13);
        assert_eq!(items[1].count, 3);
        assert!(chest.custom_name.is_none());
    }

    #[test]
    fn decode_chest_empty_nbt_falls_back_to_default() {
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Chest, &Nbt::None);
        let BlockEntityData::Chest(chest) = decoded else {
            panic!("expected Chest, got {decoded:?}");
        };
        assert!(chest.items.is_none());
        assert!(chest.custom_name.is_none());
    }

    #[test]
    fn decode_sign_messages_and_color() {
        let mut msg_compounds = Vec::with_capacity(4);
        for i in 0..4 {
            let mut c = NbtCompound::new();
            c.insert("text", NbtTag::String(format!("line {i}").into()));
            msg_compounds.push(c);
        }
        let mut front = NbtCompound::new();
        front.insert("messages", NbtTag::List(NbtList::Compound(msg_compounds)));
        front.insert("color", NbtTag::String("yellow".into()));
        front.insert("has_glowing_text", NbtTag::Byte(1));

        let mut compound = NbtCompound::new();
        compound.insert("front_text", NbtTag::Compound(front));
        compound.insert("is_waxed", NbtTag::Byte(0));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Sign, &nbt);
        let BlockEntityData::Sign(sign) = decoded else {
            panic!("expected Sign, got {decoded:?}");
        };
        let front = sign.front_text.expect("front face decoded");
        assert_eq!(front.color, "yellow");
        assert!(front.has_glowing_text);
        // FormattedText 的 to_string 会把 plain text 字段 collapse 出来。
        assert_eq!(front.messages[0].to_string(), "line 0");
        assert_eq!(front.messages[3].to_string(), "line 3");
        assert!(sign.back_text.is_none());
        assert_eq!(sign.is_waxed, Some(false));
    }

    #[test]
    fn decode_banner_pattern_stack() {
        let mut p0 = NbtCompound::new();
        p0.insert("pattern", NbtTag::String("minecraft:square_bottom_left".into()));
        p0.insert("color", NbtTag::String("white".into()));
        let mut p1 = NbtCompound::new();
        p1.insert("pattern", NbtTag::String("minecraft:cross".into()));
        p1.insert("color", NbtTag::String("red".into()));

        let mut compound = NbtCompound::new();
        compound.insert("patterns", NbtTag::List(NbtList::Compound(vec![p0, p1])));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Banner, &nbt);
        let BlockEntityData::Banner(banner) = decoded else {
            panic!("expected Banner, got {decoded:?}");
        };
        let patterns = banner.patterns.expect("patterns decoded");
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0].pattern.to_string(), "minecraft:square_bottom_left");
        assert_eq!(patterns[0].color, "white");
        assert_eq!(patterns[1].color, "red");
        assert!(banner.custom_name.is_none());
    }

    #[test]
    fn decode_skull_with_legacy_skullowner() {
        // 老 schema：SkullOwner.Properties.textures[0].Value
        // 一个最小的 vanilla skin properties value（base64 of {"textures":{"SKIN":{"url":"https://example.test/abc"}}}）。
        let value = "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHBzOi8vZXhhbXBsZS50ZXN0L2FiYyJ9fX0=";

        let mut texture_entry = NbtCompound::new();
        texture_entry.insert("Value", NbtTag::String(value.into()));
        let mut props = NbtCompound::new();
        props.insert("textures", NbtTag::List(NbtList::Compound(vec![texture_entry])));

        let mut owner = NbtCompound::new();
        owner.insert("Name", NbtTag::String("Notch".into()));
        owner.insert("Id", NbtTag::IntArray(vec![1, 2, 3, 4]));
        owner.insert("Properties", NbtTag::Compound(props));

        let mut compound = NbtCompound::new();
        compound.insert("SkullOwner", NbtTag::Compound(owner));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Skull, &nbt);
        let BlockEntityData::Skull(skull) = decoded else {
            panic!("expected Skull, got {decoded:?}");
        };
        assert_eq!(skull.profile_name.as_deref(), Some("Notch"));
        assert!(skull.profile_uuid.is_some());
        assert_eq!(
            skull.texture_url.as_deref(),
            Some("https://example.test/abc")
        );
    }

    #[test]
    fn decode_skull_with_new_profile_schema() {
        let value = "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHBzOi8vZXhhbXBsZS50ZXN0L2RlZiJ9fX0=";

        let mut prop = NbtCompound::new();
        prop.insert("name", NbtTag::String("textures".into()));
        prop.insert("value", NbtTag::String(value.into()));

        let mut profile = NbtCompound::new();
        profile.insert("name", NbtTag::String("Steve".into()));
        profile.insert("id", NbtTag::IntArray(vec![10, 20, 30, 40]));
        profile.insert("properties", NbtTag::List(NbtList::Compound(vec![prop])));

        let mut compound = NbtCompound::new();
        compound.insert("profile", NbtTag::Compound(profile));
        compound.insert("note_block_sound", NbtTag::String("minecraft:block.note_block.bell".into()));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Skull, &nbt);
        let BlockEntityData::Skull(skull) = decoded else {
            panic!("expected Skull, got {decoded:?}");
        };
        assert_eq!(skull.profile_name.as_deref(), Some("Steve"));
        assert_eq!(
            skull.note_block_sound.as_ref().map(|i| i.to_string()),
            Some("minecraft:block.note_block.bell".to_owned())
        );
        assert_eq!(
            skull.texture_url.as_deref(),
            Some("https://example.test/def")
        );
    }

    #[test]
    fn decode_bell_custom_name() {
        // 1.20.5+ vanilla schema：CustomName 是 chat-component compound
        // （`{"text":"..."}` 写进 NBT compound，而不是 JSON string）。
        let mut name_compound = NbtCompound::new();
        name_compound.insert("text", NbtTag::String("Liberty Bell".into()));

        let mut compound = NbtCompound::new();
        compound.insert("CustomName", NbtTag::Compound(name_compound));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Bell, &nbt);
        let BlockEntityData::Bell(bell) = decoded else {
            panic!("expected Bell, got {decoded:?}");
        };
        let name = bell.custom_name.expect("custom name decoded");
        assert_eq!(name.to_string(), "Liberty Bell");
    }

    #[test]
    fn decode_lectern_with_book() {
        let mut book = NbtCompound::new();
        book.insert("id", NbtTag::String("minecraft:written_book".into()));
        book.insert("count", NbtTag::Int(1));
        book.insert("Slot", NbtTag::Byte(0));

        let mut compound = NbtCompound::new();
        compound.insert("Book", NbtTag::Compound(book));
        compound.insert("Page", NbtTag::Int(7));
        compound.insert("HasBook", NbtTag::Byte(1));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Lectern, &nbt);
        let BlockEntityData::Lectern(lectern) = decoded else {
            panic!("expected Lectern, got {decoded:?}");
        };
        let book = lectern.book.expect("book decoded");
        assert_eq!(book.id.to_string(), "minecraft:written_book");
        assert_eq!(book.count, 1);
        assert_eq!(lectern.page, Some(7));
        assert_eq!(lectern.has_book, Some(true));
    }

    #[test]
    fn decode_lectern_empty_has_no_book() {
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Lectern, &Nbt::None);
        let BlockEntityData::Lectern(lectern) = decoded else {
            panic!("expected Lectern, got {decoded:?}");
        };
        assert!(lectern.book.is_none());
        assert_eq!(lectern.page, None);
        assert_eq!(lectern.has_book, None);
    }

    #[test]
    fn decode_brewing_stand_items_and_progress() {
        let mut bottle = NbtCompound::new();
        bottle.insert("Slot", NbtTag::Byte(0));
        bottle.insert("id", NbtTag::String("minecraft:potion".into()));
        bottle.insert("count", NbtTag::Int(1));

        let mut ingredient = NbtCompound::new();
        ingredient.insert("Slot", NbtTag::Byte(3));
        ingredient.insert("id", NbtTag::String("minecraft:nether_wart".into()));
        ingredient.insert("count", NbtTag::Int(2));

        let mut fuel = NbtCompound::new();
        fuel.insert("Slot", NbtTag::Byte(4));
        fuel.insert("id", NbtTag::String("minecraft:blaze_powder".into()));
        fuel.insert("count", NbtTag::Int(20));

        let mut compound = NbtCompound::new();
        compound.insert(
            "Items",
            NbtTag::List(NbtList::Compound(vec![bottle, ingredient, fuel])),
        );
        compound.insert("BrewTime", NbtTag::Short(123));
        compound.insert("Fuel", NbtTag::Byte(15));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::BrewingStand, &nbt);
        let BlockEntityData::BrewingStand(stand) = decoded else {
            panic!("expected BrewingStand, got {decoded:?}");
        };
        let items = stand.items.expect("items decoded");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].slot, 0);
        assert_eq!(items[0].id.to_string(), "minecraft:potion");
        assert_eq!(items[1].slot, 3);
        assert_eq!(items[2].id.to_string(), "minecraft:blaze_powder");
        assert_eq!(stand.brew_time, Some(123));
        assert_eq!(stand.fuel, Some(15));
        assert!(stand.custom_name.is_none());
    }

    #[test]
    fn decode_end_portal_is_marker() {
        // vanilla 端口实体 NBT 通常就是空 compound。
        let nbt = wrap(NbtCompound::new());
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::EndPortal, &nbt);
        assert_eq!(decoded, BlockEntityData::EndPortal);
        assert_eq!(decoded.kind(), BlockEntityKind::EndPortal);

        // Nbt::None 也走 marker path。
        let decoded2 = BlockEntityData::from_nbt(BlockEntityKind::EndPortal, &Nbt::None);
        assert_eq!(decoded2, BlockEntityData::EndPortal);
    }

    #[test]
    fn decode_shulker_box_items_and_color() {
        let mut item0 = NbtCompound::new();
        item0.insert("Slot", NbtTag::Byte(0));
        item0.insert("id", NbtTag::String("minecraft:redstone".into()));
        item0.insert("count", NbtTag::Int(64));

        let mut compound = NbtCompound::new();
        compound.insert("Items", NbtTag::List(NbtList::Compound(vec![item0])));
        // 14 = red（白=0…黑=15，按 DyeColor enum 排序）。
        compound.insert("Color", NbtTag::Byte(14));

        let nbt = wrap(compound);
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::ShulkerBox, &nbt);
        let BlockEntityData::ShulkerBox(shulker) = decoded else {
            panic!("expected ShulkerBox, got {decoded:?}");
        };
        let items = shulker.items.expect("items decoded");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.to_string(), "minecraft:redstone");
        assert_eq!(shulker.color, Some(14));
    }

    #[test]
    fn decode_shulker_box_without_color() {
        // vanilla server 不写 Color；color 应当为 None，渲染端按 block id 决定。
        let nbt = wrap(NbtCompound::new());
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::ShulkerBox, &nbt);
        let BlockEntityData::ShulkerBox(shulker) = decoded else {
            panic!("expected ShulkerBox, got {decoded:?}");
        };
        assert!(shulker.items.is_none());
        assert!(shulker.color.is_none());
    }

    #[test]
    fn unknown_kind_round_trips_kind() {
        let nbt = wrap(NbtCompound::new());
        let decoded = BlockEntityData::from_nbt(BlockEntityKind::Conduit, &nbt);
        assert_eq!(decoded, BlockEntityData::Unknown { kind: BlockEntityKind::Conduit });
        assert_eq!(decoded.kind(), BlockEntityKind::Conduit);
    }

    #[test]
    fn base64_decoder_roundtrip() {
        // 标准用例：vanilla mojang texture 字段几乎都是 4 的倍数 + `=` padding。
        let plain = b"hello world";
        // base64("hello world") = "aGVsbG8gd29ybGQ="
        let decoded = base64_decode(b"aGVsbG8gd29ybGQ=").expect("decode ok");
        assert_eq!(decoded, plain);

        // 非整字节边界：3 字节输入 → 4 字符 base64。
        // base64("foo") = "Zm9v"
        let decoded2 = base64_decode(b"Zm9v").expect("decode ok");
        assert_eq!(decoded2, b"foo");

        // 异常字符要 fail-safe（不 panic、返回 None）。
        assert!(base64_decode(b"!!").is_none());
    }
}
