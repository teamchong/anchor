const assert = require("assert");
const anchor = require("@project-serum/anchor");
const serumCmn = require("@project-serum/common");
const TokenInstructions = require("@project-serum/serum").TokenInstructions;
const utils = require("./utils");

describe("Governance", () => {
  // Read the provider from the configured environmnet.
  const provider = anchor.Provider.env();

  // Configure the client to use the provider.
  anchor.setProvider(provider);

  const registry = anchor.workspace.Registry;
  const voting = anchor.workspace.Voting;
  const lockup = anchor.workspace.Lockup;

  let mint = null;
  let god = null;

  it("Sets up initial test state", async () => {
    const [_mint, _god] = await serumCmn.createMintAndVault(
      provider,
      new anchor.BN(1000000000000)
    );
    mint = _mint;
    god = _god;
  });

  let registrar = null;
  const member = new anchor.web3.Account();
  let memberAccount = null;

  it("Setups up stake state", async () => {
    registrar = new anchor.web3.Account();
    const rewardQ = new anchor.web3.Account();
    const withdrawalTimelock = new anchor.BN(4);
    const stakeRate = new anchor.BN(2);
    const rewardQLen = 170;

    // Setup registry program and global state.
    const [
      registrarSigner,
      nonce,
    ] = await anchor.web3.PublicKey.findProgramAddress(
      [registrar.publicKey.toBuffer()],
      registry.programId
    );
    const poolMint = await serumCmn.createMint(provider, registrarSigner);
    await registry.state.rpc.new({
      accounts: { lockupProgram: lockup.programId },
    });

    // Create registrar.
    await registry.rpc.initialize(
      mint,
      provider.wallet.publicKey,
      nonce,
      withdrawalTimelock,
      stakeRate,
      rewardQLen,
      {
        accounts: {
          registrar: registrar.publicKey,
          poolMint,
          rewardEventQ: rewardQ.publicKey,
          rent: anchor.web3.SYSVAR_RENT_PUBKEY,
        },
        signers: [registrar, rewardQ],
        instructions: [
          await registry.account.registrar.createInstruction(registrar),
          await registry.account.rewardQueue.createInstruction(rewardQ, 8250),
        ],
      }
    );

    const registrarAccount = await registry.account.registrar(
      registrar.publicKey
    );

    // Create member account.
    const [
      memberSigner,
      _nonce,
    ] = await anchor.web3.PublicKey.findProgramAddress(
      [registrar.publicKey.toBuffer(), member.publicKey.toBuffer()],
      registry.programId
    );
    const [mainTx, _balances] = await utils.createBalanceSandbox(
      provider,
      registrarAccount,
      memberSigner
    );
    const [lockedTx, _balancesLocked] = await utils.createBalanceSandbox(
      provider,
      registrarAccount,
      memberSigner
    );

    balances = _balances;
    balancesLocked = _balancesLocked;

    const tx = registry.transaction.createMember(_nonce, {
      accounts: {
        registrar: registrar.publicKey,
        member: member.publicKey,
        beneficiary: provider.wallet.publicKey,
        memberSigner,
        balances,
        balancesLocked,
        tokenProgram: TokenInstructions.TOKEN_PROGRAM_ID,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      },
      instructions: [await registry.account.member.createInstruction(member)],
    });
    const signers = [member, provider.wallet.payer];
    const allTxs = [mainTx, lockedTx, { tx, signers }];
    await provider.sendAll(allTxs);

    memberAccount = await registry.account.member(member.publicKey);

    const depositAmount = new anchor.BN(120);
    await registry.rpc.deposit(depositAmount, {
      accounts: {
        depositor: god,
        depositorAuthority: provider.wallet.publicKey,
        tokenProgram: TokenInstructions.TOKEN_PROGRAM_ID,
        vault: memberAccount.balances.vault,
        beneficiary: provider.wallet.publicKey,
        member: member.publicKey,
      },
    });

    const stakeAmount = new anchor.BN(10);
    await registry.rpc.stake(stakeAmount, false, {
      accounts: {
        // Stake instance.
        registrar: registrar.publicKey,
        rewardEventQ: rewardQ.publicKey,
        poolMint,
        // Member.
        member: member.publicKey,
        beneficiary: provider.wallet.publicKey,
        balances,
        balancesLocked,
        // Program signers.
        memberSigner,
        registrarSigner,
        // Misc.
        clock: anchor.web3.SYSVAR_CLOCK_PUBKEY,
        tokenProgram: TokenInstructions.TOKEN_PROGRAM_ID,
      },
    });

    const vault = await serumCmn.getTokenAccount(
      provider,
      memberAccount.balances.vault
    );
    const vaultStake = await serumCmn.getTokenAccount(
      provider,
      memberAccount.balances.vaultStake
    );
    const spt = await serumCmn.getTokenAccount(
      provider,
      memberAccount.balances.spt
    );

    assert.ok(vault.amount.eq(new anchor.BN(100)));
    assert.ok(vaultStake.amount.eq(new anchor.BN(20)));
    assert.ok(spt.amount.eq(new anchor.BN(10)));
  });

  // Voting tests start here.

  let governor = new anchor.web3.Account();
  let governorSigner = null;
  let pollQ = new anchor.web3.Account();
  let proposalQ = new anchor.web3.Account();
  let time = new anchor.BN(60);
  const pollPrice = new anchor.BN(10 * 10 ** 6);
  const proposalPrice = new anchor.BN(10000 * 10 ** 6);
  let governorAccount = null;

  it("Creates a governor", async () => {
    const [
      _governorSigner,
      nonce,
    ] = await anchor.web3.PublicKey.findProgramAddress(
      [governor.publicKey.toBuffer()],
      voting.programId
    );
    governorSigner = _governorSigner;
    await voting.rpc.createGovernor(
      mint,
      time,
      nonce,
      pollPrice,
      proposalPrice,
      150,
      {
        accounts: {
          governor: governor.publicKey,
          pollQ: pollQ.publicKey,
          proposalQ: proposalQ.publicKey,
          registrar: registrar.publicKey,
          rent: anchor.web3.SYSVAR_RENT_PUBKEY,
        },
        instructions: [
          await voting.account.govQueue.createInstruction(pollQ, 8250),
          await voting.account.govQueue.createInstruction(proposalQ, 8250),
          await voting.account.governor.createInstruction(governor),
        ],
        signers: [pollQ, proposalQ, governor],
      }
    );

    governorAccount = await voting.account.governor(governor.publicKey);

    assert.ok(governorAccount.registrar.equals(registrar.publicKey));
    assert.ok(governorAccount.nonce == nonce);
    assert.ok(governorAccount.time.eq(time));
    assert.ok(governorAccount.pollQ.equals(pollQ.publicKey));
    assert.ok(governorAccount.proposalQ.equals(proposalQ.publicKey));
    assert.ok(governorAccount.pollPrice.eq(pollPrice));
    assert.ok(governorAccount.proposalPrice.eq(proposalPrice));
  });

  const poll = new anchor.web3.Account();
  const pollVault = new anchor.web3.Account();
  let pollSigner = null;

  it("Creates a poll", async () => {
    const msg = "This is a test";
    const options = ["asdf", "qwer", "zcxv"];
    const endTs = new anchor.BN(Date.now() / 1000 + 30);
    const [_pollSigner, nonce] = await anchor.web3.PublicKey.findProgramAddress(
      [poll.publicKey.toBuffer()],
      voting.programId
    );
    pollSigner = _pollSigner;

    await voting.rpc.createPoll(msg, options, endTs, nonce, {
      accounts: {
        poll: poll.publicKey,
        governor: governor.publicKey,
        pollQ: pollQ.publicKey,
        depositor: god,
        depositorAuthority: voting.provider.wallet.publicKey,
        vault: pollVault.publicKey,
        pollSigner,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
        clock: anchor.web3.SYSVAR_CLOCK_PUBKEY,
        tokenProgram: TokenInstructions.TOKEN_PROGRAM_ID,
      },
      instructions: [
        await voting.account.poll.createInstruction(poll, 2000),
        ...(await serumCmn.createTokenAccountInstrs(
          provider,
          pollVault.publicKey,
          mint,
          pollSigner
        )),
      ],
      signers: [poll, pollVault],
    });

    const pollAccount = await voting.account.poll(poll.publicKey);

    assert.ok(pollAccount.governor.equals(governor.publicKey));
    assert.ok(pollAccount.msg === "This is a test");
    assert.ok(pollAccount.startTs.gt(new anchor.BN(0)));
    assert.ok(pollAccount.endTs.eq(endTs));
    assert.deepEqual(pollAccount.options, options);
    assert.ok(pollAccount.nonce === nonce);
    assert.ok(pollAccount.vault.equals(pollVault.publicKey));

    const pollQueue = await voting.account.govQueue(governorAccount.pollQ);
    assert.ok(pollQueue.proposals[0].equals(poll.publicKey));
  });

  it("Votes for a poll", async () => {
    const vote = new anchor.web3.Account();
    const preImage = Buffer.concat([
      member.publicKey.toBuffer(),
      poll.publicKey.toBuffer(),
    ]);
    const seed = new Uint8Array(
      await anchor.utils.sha256(new Buffer([0]), { outputFormat: "buffer" })
    );
    console.log("seed", seed);
    await voting.rpc.votePoll(1, {
      accounts: {
        governor: governor.publicKey,
        poll: poll.publicKey,
        vote: vote.publicKey,
        stake: {
          member: member.publicKey,
          memberSpt: memberAccount.balances.spt,
          memberSptLocked: memberAccount.balancesLocked.spt,
        },
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
        clock: anchor.web3.SYSVAR_CLOCK_PUBKEY,
      },
      signers: [vote],
      instructions: [await voting.account.vote.createInstruction(vote)],
    });

    const pollAccount = await voting.account.poll(poll.publicKey);
    assert.deepEqual(
      pollAccount.voteWeights.map((v) => v.toNumber()),
      [0, 10, 0]
    );

    const voteAccount = await voting.account.vote(vote.publicKey);
    assert.ok(voteAccount.burned);
  });

  it("Creates a proposal", async () => {
    // todo
  });
});
