use tir::helpers::operation;

operation! {
    SectionOp {
        name: "section",
        dialect: "asm",
        regions: R {
            body: Region {}
        }
    }
}

operation! {
    BlockEndOp {
        name: "block_end",
        dialect: "asm",
    }
}
