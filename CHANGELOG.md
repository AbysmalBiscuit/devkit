# Changelog

## [0.14.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.6...v0.14.0) (2026-09-03)


### ⚠ BREAKING CHANGES

* **config:** defaults.baseline_path is removed. Set defaults.baseline_dir to the directory baselines are created under, and remove the old baseline checkout with git worktree remove --force. A baseline_path in a project config is now an error; one in the personal config warns and is ignored.
* **config:** defaults.pr_base defaults to main instead of staging. Projects that target staging must set it explicitly.
* **config:** `devrun config` is removed. Use `devkit config`.
* **issue:** `issue review request` no longer opens PRs. Use `issue pr create` first. The flags --base, --pr-title and --pr-body moved to that command.

### Features

* **config:** add defaults.baseline_dir ([dcc61c9](https://github.com/AbysmalBiscuit/devkit/commit/dcc61c99de990a50070b3f89516950d635698dc9))
* **config:** add defaults.pr_create_state ([271ad01](https://github.com/AbysmalBiscuit/devkit/commit/271ad01476dddcdb6ea31ddb6ae967a24c7ffc8a))
* **config:** add the issue end hook keys ([be423a4](https://github.com/AbysmalBiscuit/devkit/commit/be423a4ec522df863a821d51056216e516c5b7ff))
* **config:** default pr_base to main ([93f8ef7](https://github.com/AbysmalBiscuit/devkit/commit/93f8ef736563bef28c6d01ec9c54c3c8967753ea))
* **config:** derive worktree_root from the primary checkout ([a8c2720](https://github.com/AbysmalBiscuit/devkit/commit/a8c2720ee7dc8509c06ccd6982987281eb56ad2e))
* **config:** make every [defaults] key optional ([e81a45c](https://github.com/AbysmalBiscuit/devkit/commit/e81a45c0f02875fe61fc283abeca3f1111984f4b))
* **config:** replace baseline_path with baseline_dir ([c610452](https://github.com/AbysmalBiscuit/devkit/commit/c6104524fb7e47cac04b3eaa134c391d9d4e660c))
* **config:** report the layers and what each value overrides ([8accac7](https://github.com/AbysmalBiscuit/devkit/commit/8accac7100fb1883e69b29b15731b3a1a280475b))
* **devrun:** add a clickable url column to devrun status ([61d8727](https://github.com/AbysmalBiscuit/devkit/commit/61d8727a98d5e2d6b3fbb2a3cafdfb8943846ce6))
* **devrun:** name configured apps when up has none ([65c4889](https://github.com/AbysmalBiscuit/devkit/commit/65c4889808c2a0ca64fd9641006c7bf0012cc79c))
* **doctor:** report unreferenced baselines ([c8bf010](https://github.com/AbysmalBiscuit/devkit/commit/c8bf01056fc38817bbfd9083008399d5bc6124ec))
* **github:** carry is_draft on PrBrief ([b660940](https://github.com/AbysmalBiscuit/devkit/commit/b660940a3352561599b526d4d42f36da78d49bb2))
* **help:** add the verbosity decision ([753fd6f](https://github.com/AbysmalBiscuit/devkit/commit/753fd6f73274f26bf7cdf35c27f903ad47039145))
* **help:** print the command tree when stdout is not a tty ([e18651f](https://github.com/AbysmalBiscuit/devkit/commit/e18651f2ad4053c60b4cc7bf9e0be43f764aa1c8))
* **help:** render the command tree ([f945591](https://github.com/AbysmalBiscuit/devkit/commit/f945591915ce7df1f6c97492854d5c7e4452c350))
* **help:** resolve the target node through clap ([2d52296](https://github.com/AbysmalBiscuit/devkit/commit/2d5229654d061acbff6e599242285c8ae9142d9c))
* **issue:** add pr create ([8d884b9](https://github.com/AbysmalBiscuit/devkit/commit/8d884b97d18b1c4de861adef83648cd24d7ab868))
* **issue:** add pr ready with the reviewer gate ([8d52785](https://github.com/AbysmalBiscuit/devkit/commit/8d52785d689a507bec125db55d96b557faf71766))
* **issue:** add the pr command group ([69a952a](https://github.com/AbysmalBiscuit/devkit/commit/69a952af64dd86998e35f1f1f854997c5b142a7b))
* **issue:** keep baselines out of worktree discovery ([a71478f](https://github.com/AbysmalBiscuit/devkit/commit/a71478f0f859f4326fa534990e1c42a7122dbf7b))
* **issue:** reclaim a baseline when its last referencer ends ([0c22e2d](https://github.com/AbysmalBiscuit/devkit/commit/0c22e2dfad14e0d394563be91dcb66c2f4288148))
* **issue:** record the baseline a worktree compares against ([bb21e0b](https://github.com/AbysmalBiscuit/devkit/commit/bb21e0b3a0d785260ed9387f0bb67b1d4f39958a))
* **issue:** render drafts in triage and the reviewer queue ([ebf7eb9](https://github.com/AbysmalBiscuit/devkit/commit/ebf7eb9541802b1bc5488af93dd78666ccb07503))
* **issue:** require an existing PR in review request ([6e65a2d](https://github.com/AbysmalBiscuit/devkit/commit/6e65a2d3bd510e200796f5eb4db11c8b1ad5fd03))
* **issue:** resolve the baseline target from origin/HEAD ([1869d1c](https://github.com/AbysmalBiscuit/devkit/commit/1869d1cf29515ee6a404909c4a7e503cf205a8c0))
* **issue:** run after_end hooks once per end run ([31552f1](https://github.com/AbysmalBiscuit/devkit/commit/31552f1f314769f1e6d137985183d9dbebd376f8))
* **issue:** run after_worktree_remove hooks on end ([5abf11a](https://github.com/AbysmalBiscuit/devkit/commit/5abf11a2e859f983e4e425cca1ae2fd214981ccc))
* **issue:** tag PrStatus::Unique with is_draft ([4f7ffc4](https://github.com/AbysmalBiscuit/devkit/commit/4f7ffc4ed6d189ef71d2261ab086add4264494b8))
* **run:** add baseline slot and directory locks ([8babb56](https://github.com/AbysmalBiscuit/devkit/commit/8babb569e79b19b76651e1c3be062d08aad7f5f6))
* **run:** add devrun baseline list and prune ([d5516a1](https://github.com/AbysmalBiscuit/devkit/commit/d5516a162a865cfef51d53a6de6257f6695fde11))
* **run:** add the baseline marker file ([0ffb50f](https://github.com/AbysmalBiscuit/devkit/commit/0ffb50fc54b55c51812d4c9b2c470e89947fdb13))
* **run:** bootstrap a baseline worktree lazily ([f44538d](https://github.com/AbysmalBiscuit/devkit/commit/f44538df111f515b04fd4e562d089f6d8c1bc673))
* **run:** create baselines lazily instead of resetting one ([9bca405](https://github.com/AbysmalBiscuit/devkit/commit/9bca405b267edc5694b69898fa382634535f4379))
* **run:** let a sole referencer stop its own baseline ([c10b71c](https://github.com/AbysmalBiscuit/devkit/commit/c10b71c7a97828c77e982a95f430248dd4da66c9))
* **run:** pin the baseline to the worktree's fork point ([038acd6](https://github.com/AbysmalBiscuit/devkit/commit/038acd6b404a3a9cec2d2609fd8a2e787a3f9c64))
* **run:** resolve a baseline's slot from its marker ([4216d99](https://github.com/AbysmalBiscuit/devkit/commit/4216d9963fd0f989f77d129c41620ec4b13770e8))


### Bug Fixes

* **baseline:** keep the whitespace git keeps in a gitdir ([a3364b8](https://github.com/AbysmalBiscuit/devkit/commit/a3364b867f64774fb216b16da607aa8272b074d9))
* **baseline:** let a worktree repair a baseline it cannot read ([7a86947](https://github.com/AbysmalBiscuit/devkit/commit/7a8694726d8a6da584e7647d2ff9bf67849b4998))
* **baseline:** make the sweep agree with itself ([155cf63](https://github.com/AbysmalBiscuit/devkit/commit/155cf63747be14fed58eb889d86b4467adcc5357))
* **baseline:** match a baseline by identity, not by spelling ([ff6d384](https://github.com/AbysmalBiscuit/devkit/commit/ff6d384a340704e6f98e04053d13c4398f98f6d3))
* **baseline:** pin the baseline under the lock that built it ([5aca2b7](https://github.com/AbysmalBiscuit/devkit/commit/5aca2b7161c0646229a26117b9d68a089416c2ea))
* **baseline:** refuse a git directory it cannot classify ([02b4898](https://github.com/AbysmalBiscuit/devkit/commit/02b4898992c194a2e58cc1bf803d0bfb787dc1de))
* **baseline:** refuse a pin over a record it cannot read ([661db89](https://github.com/AbysmalBiscuit/devkit/commit/661db8916a8e4852b60d316d4fd6ef07ba7d1720))
* **baseline:** refuse a rebuild of a tree another worktree pins ([b50bda5](https://github.com/AbysmalBiscuit/devkit/commit/b50bda5fe718f2c485718930a51cad18c34d5f34))
* **baseline:** refuse a tree whose classification failed ([60920d7](https://github.com/AbysmalBiscuit/devkit/commit/60920d7fe9d93adc80ac5768793263fa31b10a3d))
* **baseline:** refuse to delete a path that is not a baseline ([524a76e](https://github.com/AbysmalBiscuit/devkit/commit/524a76eae8e864702cbd8ce47b51e073d418a2ec))
* **baseline:** tighten the sole-referencer exemption ([1bb4a55](https://github.com/AbysmalBiscuit/devkit/commit/1bb4a559ffd4e7ee54649915724fa89191867a90))
* **cli:** drop issue's misleading partial about ([74fc5bb](https://github.com/AbysmalBiscuit/devkit/commit/74fc5bbf8eac29ce29f96c2e8a0d785b58e911e0))
* close the remaining unvalidated-input gaps ([6ad50bb](https://github.com/AbysmalBiscuit/devkit/commit/6ad50bb76e88a88c5ecd79eb7f581804d452224d))
* **completions:** stop declaring the help subtree ([399807b](https://github.com/AbysmalBiscuit/devkit/commit/399807bbdddabdf90d9e99a51cc0e4fb19a3ac25))
* **devkit-config:** stop double-escaping paths in TOML literal fixtures ([d1a004e](https://github.com/AbysmalBiscuit/devkit/commit/d1a004e8b5dc2c763af38f65b76a4a1cb47d1bf8))
* **devrun:** resolve each status row's url against its own holder's config ([c6dd519](https://github.com/AbysmalBiscuit/devkit/commit/c6dd519d6dbc7036743a3dadaf94c1a5837378e3))
* **devrun:** reuse the loaded config on status --all ([f6e5d7f](https://github.com/AbysmalBiscuit/devkit/commit/f6e5d7f3fd28b5369dc2b90fb8d71ddf117b4078))
* **doctor:** stop counting baselines a sweep will not remove ([0602859](https://github.com/AbysmalBiscuit/devkit/commit/0602859682136afd526e7d33f14a9de293615d7c))
* **doctor:** treat unreadable worktrees as blocking orphan detection ([847df99](https://github.com/AbysmalBiscuit/devkit/commit/847df9922637c90370d543e9b97a2f24dbbcc520))
* **help:** decline on an unrecognized help path ([af951ed](https://github.com/AbysmalBiscuit/devkit/commit/af951ed3cddeb5835edd1c8cd9b4d1f132162a21))
* **help:** hand the help node itself back to clap ([5a18ab0](https://github.com/AbysmalBiscuit/devkit/commit/5a18ab0a822e6c0dc6bc39cc5cbe9e1cf5181a2b))
* **help:** replace non-ascii ellipsis in doc comment with ascii ([e9aa79c](https://github.com/AbysmalBiscuit/devkit/commit/e9aa79c48220707020c6de3c0c3629e4386a4275))
* **issue:** anchor an explicit worktree path to the caller ([185a026](https://github.com/AbysmalBiscuit/devkit/commit/185a02630b160e2b8cedd8c980f33a989024081b))
* **issue:** exclude the PR author from the reviewer gate ([767c146](https://github.com/AbysmalBiscuit/devkit/commit/767c146b8d1f73f5729a5a50baa6c59cfb4b8cb8))
* **issue:** gate pr ready only on a real transition ([2a683c4](https://github.com/AbysmalBiscuit/devkit/commit/2a683c4f16bd19d0cf5b79fb648190b1361f12fe))
* **issue:** name only configured hook keys in skip warning ([6c9edf2](https://github.com/AbysmalBiscuit/devkit/commit/6c9edf206025b6755af272e9e7fa380e5976e9fe))
* **issue:** record the PR before the ready flip ([79e8784](https://github.com/AbysmalBiscuit/devkit/commit/79e8784cad1cfc51ece81eacda9ae68961e0c596))
* **issue:** refuse a removal whose working directory will not resolve ([4fd5561](https://github.com/AbysmalBiscuit/devkit/commit/4fd5561b2b65b8ebec154259c674ae55ff3cd9f6))
* **issue:** refuse a worktree placement devkit cannot derive ([9747d24](https://github.com/AbysmalBiscuit/devkit/commit/9747d24ace8ee1d9543b700ff7438d49fac54407))
* **issue:** refuse to end a worktree whose record cannot be read ([b08a7a8](https://github.com/AbysmalBiscuit/devkit/commit/b08a7a8af74e9a6aac1d71682bfc722e84bc0428))
* **issue:** render a closed draft as closed ([a70243e](https://github.com/AbysmalBiscuit/devkit/commit/a70243e6a2e2456dc90281b183deb7bdd47a4d8e))
* **issue:** render the pr body only when creating ([79fe77b](https://github.com/AbysmalBiscuit/devkit/commit/79fe77b6ad780e14693e033c043680870ed1573b))
* **issue:** resolve recipients before the ready flip ([e378a65](https://github.com/AbysmalBiscuit/devkit/commit/e378a658aabd68973b4ec2ba3d18dc6d7beb8fe8))
* **issue:** select a worktree by path identity ([37d2ae8](https://github.com/AbysmalBiscuit/devkit/commit/37d2ae85b3b223af6d92add44db656049f4e9df5))
* **issue:** stop a baseline's servers before end removes the worktree ([7d73461](https://github.com/AbysmalBiscuit/devkit/commit/7d73461e0ac867195d6ccd2cf576b961e22494b6))
* **mcp:** scope devrun.down to the server's own worktree ([2ff3d4c](https://github.com/AbysmalBiscuit/devkit/commit/2ff3d4c8bde164d0aabd252eeb4f1321224ebf18))
* **run:** give baseline bootstrap the prefix and worktree keys ([2de3ed7](https://github.com/AbysmalBiscuit/devkit/commit/2de3ed7b2b0679928f4a1c0221c22eb2b6de38ee))
* **run:** keep another worktree's config notes off this terminal ([2eecc8c](https://github.com/AbysmalBiscuit/devkit/commit/2eecc8c4eaccace31c0d70b2b13f52ecd48dfcc9))
* **run:** make a baseline dry run read-only ([e05754b](https://github.com/AbysmalBiscuit/devkit/commit/e05754b14bc74253101896b117b5f3e084f02a25))
* **run:** spare a shared baseline's servers ([38874cb](https://github.com/AbysmalBiscuit/devkit/commit/38874cb37ff5e19304dd7698d9be96e91d345d2f))
* **run:** treat an unreadable baseline marker as unusable ([7c7ab1b](https://github.com/AbysmalBiscuit/devkit/commit/7c7ab1b44cd2179322934f9e607784473011fe53))
* **sys:** count a process this user cannot signal as alive ([1c93e70](https://github.com/AbysmalBiscuit/devkit/commit/1c93e70069fc7d8bf10aa4df9e099632e861af82))
* **test:** move the fixtures onto baseline_dir ([a46ac7a](https://github.com/AbysmalBiscuit/devkit/commit/a46ac7ab1acb44e9f1f7ca2726f265d45bfc0c8b))
* **ui:** only pin and link a url column when there is room for it ([355d429](https://github.com/AbysmalBiscuit/devkit/commit/355d42963a3c05b6763159a4efc2f5a139495f08))
* **ui:** pin and truncate linked url cells so they never split ([bd068e8](https://github.com/AbysmalBiscuit/devkit/commit/bd068e89eaa8886ea083058b6fe19fa947497692))
* **ui:** stop a wide plain column from suppressing the link ([ae0a5a1](https://github.com/AbysmalBiscuit/devkit/commit/ae0a5a141c5bee0c41ea0be0d16d8c44456fd6b4))


### Performance Improvements

* **config:** parse each layer once, not twice ([9750ac6](https://github.com/AbysmalBiscuit/devkit/commit/9750ac6d5eb07e09fa1626790cfd7f07cf262a9e))
* **config:** resolve the checkout root once ([8097014](https://github.com/AbysmalBiscuit/devkit/commit/809701453f90963e124294bf00ac71928fef3992))


### Code Refactoring

* **config:** move config off run and onto devkit ([32fdac6](https://github.com/AbysmalBiscuit/devkit/commit/32fdac67ffbceeeee612cea727878bb014ac0bfc))

## [0.13.6](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.5...v0.13.6) (2026-08-31)


### Features

* **config:** add rendered-name length limits ([6a4c424](https://github.com/AbysmalBiscuit/devkit/commit/6a4c4247c1eae97e602919edb38d77f6aa654736))
* **config:** add the [parallelism] table ([b5fb874](https://github.com/AbysmalBiscuit/devkit/commit/b5fb874c0d48cf9913a56592068620560967debd))
* **config:** add the preserve table ([1eb7f4d](https://github.com/AbysmalBiscuit/devkit/commit/1eb7f4d116d94f2005d317f41615c8d2fef9f4ca))
* **config:** size the shared pool from [parallelism] ([d3538cf](https://github.com/AbysmalBiscuit/devkit/commit/d3538cfe4bb6f57dd3acf84d96dbc7429c0ad431))
* **issue:** add short_slug for worktree directories ([01c82d3](https://github.com/AbysmalBiscuit/devkit/commit/01c82d36e699c3e706c37fbeb020103eec1d530c))
* **issue:** group sync-includes file lists ([141cc4c](https://github.com/AbysmalBiscuit/devkit/commit/141cc4ca909949030551d1be76e3714475085e18))
* **issue:** measure a template's room for a slug ([fda4710](https://github.com/AbysmalBiscuit/devkit/commit/fda471023e64d66b91a7943336c5edfe8e971089))
* **issue:** name symlinks as links in a sync-includes dry run ([6e63370](https://github.com/AbysmalBiscuit/devkit/commit/6e633704104ef33d76a9c372e4b5e55d52568de9))
* **issue:** preserve worktree files before removing them ([0df2bef](https://github.com/AbysmalBiscuit/devkit/commit/0df2bef5fd45e9f0aada6c76a5c40f7bd2c2c59f))
* **issue:** report created symlinks beside copied files ([094fee8](https://github.com/AbysmalBiscuit/devkit/commit/094fee89b22d52ea80f8829a37683c20935d763a))
* **issue:** resolve and validate preserve entries ([7187b3c](https://github.com/AbysmalBiscuit/devkit/commit/7187b3cc8472332a2a56aeee0c46b7305ea8b446))
* **issue:** return the config load outcome from select ([1ae0ab1](https://github.com/AbysmalBiscuit/devkit/commit/1ae0ab169f1702d1edd605671566c299812e9e48))
* **issue:** show progress while copying worktree includes ([a1d59f6](https://github.com/AbysmalBiscuit/devkit/commit/a1d59f637aaa7fbad34a6f7501cd71269f6edf1d))
* **pool:** add the workspace's shared worker pool ([0da87a9](https://github.com/AbysmalBiscuit/devkit/commit/0da87a9a16e3cf173c2f3f97ed649179e07f7d70))
* **progress:** let steps draw sub-progress ([a05ae31](https://github.com/AbysmalBiscuit/devkit/commit/a05ae31857859f971084c0b593d182a4a143d9af))
* **progress:** settle a step failed without a Result ([ae94517](https://github.com/AbysmalBiscuit/devkit/commit/ae945176bae9ef5edb66412054e19a601fab7df3))
* **sys:** add a symlink primitive to the platform boundary ([9b04817](https://github.com/AbysmalBiscuit/devkit/commit/9b0481771b30c23936ad6e231514908f495a2fae))
* **worktree:** add copy_out for archiving a worktree ([d7af7b2](https://github.com/AbysmalBiscuit/devkit/commit/d7af7b2c083576fdd21d765e9635d6fdef0d3909))
* **worktree:** derive a pattern's literal walk root ([9c0fd6e](https://github.com/AbysmalBiscuit/devkit/commit/9c0fd6ebb7687bfb3603d6f2ef1645ec25315e92))
* **worktree:** report progress from the include copy ([23f3875](https://github.com/AbysmalBiscuit/devkit/commit/23f38754718fed63e6f7c3cc98b0ad3df7a96978))
* **worktree:** report progress from the include walk ([b05a4e8](https://github.com/AbysmalBiscuit/devkit/commit/b05a4e8f1afd2bbde6401ce984cca36c174a43ed))
* **worktree:** reproduce a matched symlink instead of its contents ([5fb97dd](https://github.com/AbysmalBiscuit/devkit/commit/5fb97dd48dfa3277bc5d90f7eda8ab533fcf8711))
* **worktree:** tell cheap includes from expensive ones ([4fd9b56](https://github.com/AbysmalBiscuit/devkit/commit/4fd9b562f4d6de8d72cb015789b4cf2da35efbaa))


### Bug Fixes

* **config:** size the shared pool at the one config door ([83614e2](https://github.com/AbysmalBiscuit/devkit/commit/83614e2618b84b5aec323cf805fb5e4ff672fa9f))
* **issue:** ask the filesystem whether a preserve destination is inside ([823fa70](https://github.com/AbysmalBiscuit/devkit/commit/823fa70bf2629706580dd807c7ad9a608494e57c))
* **issue:** cap the checkout-pr directory name ([0937531](https://github.com/AbysmalBiscuit/devkit/commit/09375310e80a19a6d9a9fed94afd149b97af12b7))
* **issue:** clamp branch budget without slug ([f5a9f7f](https://github.com/AbysmalBiscuit/devkit/commit/f5a9f7fbba1a60a00164c6c3abe953f4b1509713))
* **issue:** correct end's preserve reporting and summary sweep ([4fc1f85](https://github.com/AbysmalBiscuit/devkit/commit/4fc1f85ff8b1c4d35405ecdcbb4588080be069ad))
* **issue:** name the config key in budget errors ([e19b86d](https://github.com/AbysmalBiscuit/devkit/commit/e19b86d219adc329fc3c4892b558ce86d180d2d1))
* **issue:** pin include sub-step arithmetic and wording ([58fb1c9](https://github.com/AbysmalBiscuit/devkit/commit/58fb1c9480a2a7bbbeccdc78b38219a945e67200))
* **issue:** report occupied symlinks by name, not by subtraction ([a256db2](https://github.com/AbysmalBiscuit/devkit/commit/a256db2a0635f7c3bbd6711a798ceacea2026173))
* **issue:** tighten end's config and preserve gates ([c7ee838](https://github.com/AbysmalBiscuit/devkit/commit/c7ee838ec29d01f7b80ad144019f773bec991a73))
* **progress:** cover detail rendering, degrade poisoned locks ([3a20483](https://github.com/AbysmalBiscuit/devkit/commit/3a20483fc74bd20f7e91a93fb1069c92f05cfbe0))
* **progress:** gate substep and align its column ([ba890d7](https://github.com/AbysmalBiscuit/devkit/commit/ba890d71e53fb73640f17780605e912358cac00d))
* **test:** strengthen pool configuration assertion to pin actual width ([9b989a4](https://github.com/AbysmalBiscuit/devkit/commit/9b989a49757cad570f06c59cc82dca0aae481435))
* **worktree:** archive the root for a pattern of only dots ([2a4b10b](https://github.com/AbysmalBiscuit/devkit/commit/2a4b10b46f01bcd0888b86198f52d91302033802))
* **worktree:** bound jwalk descent to what a pattern can match ([fdd071a](https://github.com/AbysmalBiscuit/devkit/commit/fdd071aedfe31b47264a06f65cedc83bc18b5138))
* **worktree:** classify jwalk errors correctly ([a035cbe](https://github.com/AbysmalBiscuit/devkit/commit/a035cbe668cd66806ec8b12ee1067de59f7c0457))
* **worktree:** degrade Prune when a prefix will not compile ([a86a4d9](https://github.com/AbysmalBiscuit/devkit/commit/a86a4d92fb1b9bb415aeea42112db0007de1ea72))
* **worktree:** drain the walk on the calling thread ([bf172e7](https://github.com/AbysmalBiscuit/devkit/commit/bf172e77a43cf1d219acb9be47b36cdee5000e85))
* **worktree:** drop an empty include pattern before the join ([39c3f30](https://github.com/AbysmalBiscuit/devkit/commit/39c3f30044b930ea1221bcc8a90a93eddc7db924))
* **worktree:** drop change-relative phrasing in doc comment ([5185e36](https://github.com/AbysmalBiscuit/devkit/commit/5185e3626978d1b95feb1763a54e914caa502a8b))
* **worktree:** gate a test helper to the platform that uses it ([52b4f61](https://github.com/AbysmalBiscuit/devkit/commit/52b4f616612c9fdb50ee7d4439f88647dd0ab1b8))
* **worktree:** name the target of an unresolvable link ([dd11c2f](https://github.com/AbysmalBiscuit/devkit/commit/dd11c2fb54b23f650fe91c95d82dd12172b6fcc7))
* **worktree:** name what consumes walk_root ([5268bf4](https://github.com/AbysmalBiscuit/devkit/commit/5268bf4c0863e779f557876e4f2aa88f198cc27e))
* **worktree:** never plan the source root as a link ([cb20530](https://github.com/AbysmalBiscuit/devkit/commit/cb205304f730f503a8b8ac5d4985cf85ef0b1801))
* **worktree:** plan a matched symlink as a link, not its contents ([9565235](https://github.com/AbysmalBiscuit/devkit/commit/95652355a1e9efafc9772c2794cbf58202c4bf06))
* **worktree:** plan each file once across overlapping patterns ([6cbe052](https://github.com/AbysmalBiscuit/devkit/commit/6cbe052870971c5b63a1f56daa226a434f1b717b))
* **worktree:** read a pattern's components once, not twice ([f690ff5](https://github.com/AbysmalBiscuit/devkit/commit/f690ff5273d991ffc67a07c38fc4e822ea58374e))
* **worktree:** refuse a symlink cycle jwalk cannot see ([d8a5ac2](https://github.com/AbysmalBiscuit/devkit/commit/d8a5ac2973c9f621d6b00a51b6b75d03168d109e))
* **worktree:** refuse an include pattern that leaves the source tree ([a4b3a72](https://github.com/AbysmalBiscuit/devkit/commit/a4b3a725468743d95efda59b096cfb36ae18d3fe))
* **worktree:** report a directory the walk could not read ([bd1da6e](https://github.com/AbysmalBiscuit/devkit/commit/bd1da6e24973e921a35e6e1960fa946295237f14))
* **worktree:** reword atomic comment, add empty-slot test ([ef065b8](https://github.com/AbysmalBiscuit/devkit/commit/ef065b8083d99bcc158c1f9796e71fb904fc90a2))
* **worktree:** reword Walk's Sync doc comment ([36fa816](https://github.com/AbysmalBiscuit/devkit/commit/36fa816580777e1201727d48ffc2b02dc6794245))
* **worktree:** seed the cycle check from every ancestor of the walk root ([ad8c35a](https://github.com/AbysmalBiscuit/devkit/commit/ad8c35a608afb26d41775c763db8c27b25f296ad))
* **worktree:** stay silent on a directory deleted mid-walk ([4c308ca](https://github.com/AbysmalBiscuit/devkit/commit/4c308ca247c00d5641c2a4cd147f9e6fc4d79a6f))
* **worktree:** strip a leading ./ from an include pattern ([80eb3da](https://github.com/AbysmalBiscuit/devkit/commit/80eb3daad9cf306fc8a682aa8a003a6bfef34dc9))


### Performance Improvements

* **worktree:** copy a pattern's files in parallel ([a2c272f](https://github.com/AbysmalBiscuit/devkit/commit/a2c272f19788b30ddec9e41c0745ca0135fb3c6f))
* **worktree:** expand include patterns with a scoped jwalk ([faa49d9](https://github.com/AbysmalBiscuit/devkit/commit/faa49d9f3e6fba71b6330942361b492be0034e8c))
* **worktree:** walk and classify a directory include in parallel ([3c5e492](https://github.com/AbysmalBiscuit/devkit/commit/3c5e492294976b27da1bf1f5671cb1ed83e1cddf))

## [0.13.5](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.4...v0.13.5) (2026-08-29)


### Features

* **common:** add read-only worktree include planner ([476b5aa](https://github.com/AbysmalBiscuit/devkit/commit/476b5aa2003494b4533c6786b01731c49e519764))
* **common:** add the single git builder ([9831987](https://github.com/AbysmalBiscuit/devkit/commit/98319873ed3bd2881ca388dc2f978b7a58514273))
* **common:** answer checkout questions through the git module ([7a2a1bb](https://github.com/AbysmalBiscuit/devkit/commit/7a2a1bb104933928905cdec38824e0daa0885ae4))
* **completions:** add nushell ([1ab800e](https://github.com/AbysmalBiscuit/devkit/commit/1ab800ebc6a914093b6a94c064d829b25d3af226))
* **completions:** emit every name from one command ([f41aeaa](https://github.com/AbysmalBiscuit/devkit/commit/f41aeaa08deba4da63eb0428f6f2efad6c5ea3a2))
* **config:** add project_layers as the one layer discovery ([078b4ad](https://github.com/AbysmalBiscuit/devkit/commit/078b4adc3d9a9e42f95e3b2735caaec531647a01))
* **config:** anchor doppler_yaml to the reading checkout ([3969be6](https://github.com/AbysmalBiscuit/devkit/commit/3969be6ba5998aca11f3082c3fd4d3bbe2f77d03))
* **config:** layer the main checkout's config into a worktree ([f16e399](https://github.com/AbysmalBiscuit/devkit/commit/f16e3994ebd07b67ee8e27a1f0d8b2afe3c7300f))
* **devkit:** add install-links to create the shim hardlinks ([09ba730](https://github.com/AbysmalBiscuit/devkit/commit/09ba730cd3799d80a8ef02fba37e209570bd9831))
* **devkit:** add the shim name table and argv0 resolution ([7c6f66e](https://github.com/AbysmalBiscuit/devkit/commit/7c6f66e254aaa80ad89310cda41e8ca532f58e7b))
* **devkit:** link shim names automatically on a version change ([98d908f](https://github.com/AbysmalBiscuit/devkit/commit/98d908f263a0e44231f2549fc3d26fc8d0752d82))
* **devkit:** merge devkit-mcp into devkit mcp ([10a5bf0](https://github.com/AbysmalBiscuit/devkit/commit/10a5bf021d3f7bc667c3c9eb915c3050d5e3f65c))
* **devkit:** merge devrun into devkit run ([41327ce](https://github.com/AbysmalBiscuit/devkit/commit/41327ce29efc7b5ee6006a2a47e7395497ca1c86))
* **devkit:** merge docm into devkit docs ([32da173](https://github.com/AbysmalBiscuit/devkit/commit/32da1738e3bf1e3cf73bace29243f0d048fd862d))
* **devkit:** merge issue into devkit issue ([cab4eb5](https://github.com/AbysmalBiscuit/devkit/commit/cab4eb56e734ba68d13a687fc9a7cbb6f1ae126d))
* **devkit:** merge lockm into devkit locks ([f427014](https://github.com/AbysmalBiscuit/devkit/commit/f427014223d1a33a1622326146094f0dfac014a6))
* **devkit:** merge portm into devkit ports behind an argv0 shim ([a8fcd83](https://github.com/AbysmalBiscuit/devkit/commit/a8fcd83640d7e69610a373de61a52b3cfcc455e9))
* **devkit:** name the shim spellings in devkit --help ([b9eb629](https://github.com/AbysmalBiscuit/devkit/commit/b9eb6299b526eff56f1553dfae82b3c5efd1923e))
* **devkit:** report shim link state in doctor ([76b86be](https://github.com/AbysmalBiscuit/devkit/commit/76b86be4049abaf62964f32baef9a6c9521edfbf))
* **issue:** add sync-includes ([6e728c7](https://github.com/AbysmalBiscuit/devkit/commit/6e728c7d41848205bce02db603ddde60d06afc47))
* **issue:** exempt a dry run from the overwrite gate ([656fe8e](https://github.com/AbysmalBiscuit/devkit/commit/656fe8e3c95a8b6041fb2b64d09e29ce69e47945))
* **issue:** scope-gate sync-includes --overwrite ([503a901](https://github.com/AbysmalBiscuit/devkit/commit/503a901577d04a2ab13c02ae294fd17ccab08af7))
* **locks,docs:** inherit the main checkout's layers ([828b7fc](https://github.com/AbysmalBiscuit/devkit/commit/828b7fcffd160636c7dc37318f9eaacb4b593331))


### Bug Fixes

* **cli:** name the command a subcommand's version line reports ([f0fc77b](https://github.com/AbysmalBiscuit/devkit/commit/f0fc77b78720f84fa7cd39d580beb3d89f16d45b))
* **common:** align git::branch with the DETACHED sentinel ([3be48bf](https://github.com/AbysmalBiscuit/devkit/commit/3be48bfed10acbbdaeceaa28094d077f7f2106db))
* **common:** drain git output on threads and widen its timeouts ([6fc027c](https://github.com/AbysmalBiscuit/devkit/commit/6fc027c1fa7a978d82ee56e9924a4127c4b2fee1))
* **common:** preserve non-UTF-8 cwd bytes on Git::at ([701ce70](https://github.com/AbysmalBiscuit/devkit/commit/701ce703f8ff00fe635f3ad600faa1cc87b3ac26))
* **common:** re-check the destination at write time ([f48de33](https://github.com/AbysmalBiscuit/devkit/commit/f48de332357cb418ec06cdc6c8098b2393d8472d))
* **common:** sort the include plan's vectors ([79578c7](https://github.com/AbysmalBiscuit/devkit/commit/79578c733134f454e254eb41d9edc267c2376f48))
* **common:** stop touching disk in the non-UTF-8 cwd test ([7a05dd1](https://github.com/AbysmalBiscuit/devkit/commit/7a05dd1a711b8d8784e2871bc6402fec76847fbb))
* **completions:** hoist the powershell using statements ([f81171f](https://github.com/AbysmalBiscuit/devkit/commit/f81171f99f7524a97aa01decc8108b2828daf4a1))
* **completions:** keep clap help text ascii ([9d10628](https://github.com/AbysmalBiscuit/devkit/commit/9d10628a0c8f5c2f6bd22de4981f2bc559dca6c4))
* **config,issue:** pin baseline_path, doc the dev-dep, fix monorepo_dir ([89419bf](https://github.com/AbysmalBiscuit/devkit/commit/89419bfc644f4f3047e1bcbe1f18745ea873ffeb))
* **config:** stop parsing config above the root barrier ([9c38959](https://github.com/AbysmalBiscuit/devkit/commit/9c3895902fab250450e12149377e4208cdc300a6))
* **devkit:** close the zero-subcommand exploit and the Windows build break ([99ed3cb](https://github.com/AbysmalBiscuit/devkit/commit/99ed3cb7e332de5db76fa20f5f627f38e919a901))
* **devkit:** fix ports version parity, shim hardlink, and doc drift ([0b8af8f](https://github.com/AbysmalBiscuit/devkit/commit/0b8af8f0fe479b16f043705fb17d1e46fb7d7816))
* **devkit:** fix reap-gate test, output/README parity, and test naming ([79eb41e](https://github.com/AbysmalBiscuit/devkit/commit/79eb41e1091124b1704a8fb8610a7414e080275a))
* **devkit:** isolate probe stdin and cover the hide-filter directly ([9564921](https://github.com/AbysmalBiscuit/devkit/commit/9564921107a3890fb3b942eb77005d0131c918c2))
* **devkit:** key the links stamp on exe identity, not version alone ([be444cd](https://github.com/AbysmalBiscuit/devkit/commit/be444cdc48fc0566afc0d2387c99c2c8fed6a15d))
* **devkit:** make install-links foreign-binary detection actually safe ([fd864e4](https://github.com/AbysmalBiscuit/devkit/commit/fd864e446749b2e25167cdb26be23bb92da6aebb))
* **devkit:** make shim dispatch exhaustive at compile time ([e07d752](https://github.com/AbysmalBiscuit/devkit/commit/e07d7527d1a6ccfc15fb7a1b66a5c32941d0ce69))
* **devkit:** stop install-links racing its own automatic pass ([ad9ae81](https://github.com/AbysmalBiscuit/devkit/commit/ad9ae81d8562325162ce4a8ec3d690e6b235dc7e))
* **docs:** resolve the manifest over the shared layer stack ([62fc884](https://github.com/AbysmalBiscuit/devkit/commit/62fc8843f2cdb05966678cbbe58ca1408e8bd411))
* **doctor:** report the version each shim name resolves to ([e00b83d](https://github.com/AbysmalBiscuit/devkit/commit/e00b83d2245c0b76e8e09ef6693e81826a0c8f0c))
* **github:** resolve ssh aliases in the origin URL ([eebf0a7](https://github.com/AbysmalBiscuit/devkit/commit/eebf0a7e18046999c61f9ce4a980cdee6eb9b698))
* **hooks:** restore the missing-shim recovery path ([d751e44](https://github.com/AbysmalBiscuit/devkit/commit/d751e446e53030a563344a8b5470fa0c06bc7dea))
* **issue:** copy missing files when overwrite is declined ([7248d91](https://github.com/AbysmalBiscuit/devkit/commit/7248d91418a02177b3300031ec89722acd66220c))
* **issue:** report a worktree dirty when git cannot answer ([42c4470](https://github.com/AbysmalBiscuit/devkit/commit/42c4470c15d0d28f01c89202314959b02c1c7d9d))
* **issue:** resolve the primary checkout through git ([d598d0d](https://github.com/AbysmalBiscuit/devkit/commit/d598d0d6b85e7f7a81250d1678476b18192b4622))
* **issue:** unify review's detached-HEAD vocabulary on git::branch ([645e031](https://github.com/AbysmalBiscuit/devkit/commit/645e03118300a4602be3dc75f16cdcc3454e2d70))
* **links:** fall open when the state dir is unusable ([8ef2bc3](https://github.com/AbysmalBiscuit/devkit/commit/8ef2bc39817fc95488560dd7a02d6b89845564cc))
* **links:** keep a probed binary from linking anything ([061a536](https://github.com/AbysmalBiscuit/devkit/commit/061a536c54ee64eeef216f11d9594b3339e95e3d))
* **links:** record whether a linking pass finished ([4ecf9fb](https://github.com/AbysmalBiscuit/devkit/commit/4ecf9fbc2bfe43f4312087bdc01746c72e69aafb))
* **links:** settle shim identity on the marker probe alone ([b7b5b4b](https://github.com/AbysmalBiscuit/devkit/commit/b7b5b4bfbb4ed78dfe4a835ef455bd5a9b87d060))
* **locks:** accept a path reaching the repo by link ([f9d5910](https://github.com/AbysmalBiscuit/devkit/commit/f9d5910c24b8de6c0402f5233a094d4378bc80bf))
* **locks:** resolve the harness over the full layer stack ([43fcf81](https://github.com/AbysmalBiscuit/devkit/commit/43fcf817396c1f8c1c8e4a91d31c9f273554d6f5))
* **locks:** scope a lock for a not-yet-created path ([33cda82](https://github.com/AbysmalBiscuit/devkit/commit/33cda82eecec24acfca07d144dab64ee606f350f))
* **locks:** short-circuit enforcement on an explicit override ([1811ead](https://github.com/AbysmalBiscuit/devkit/commit/1811eadcb279f89a142b86784a8b4a8da7d839ff))
* **locks:** stop treating a broken git as no repository ([3d2212a](https://github.com/AbysmalBiscuit/devkit/commit/3d2212abcfbc1325cc50769e04369834ac4f2080))
* reconcile the shim move with main ([f2b5b24](https://github.com/AbysmalBiscuit/devkit/commit/f2b5b240dc870cf1b14b901f8b81827bb41a9a80))
* **tests:** gate the Stdio import on unix ([2acb25c](https://github.com/AbysmalBiscuit/devkit/commit/2acb25cf38667fab92240a87aa56f1ad0538f8a4))


### Performance Improvements

* **locks:** resolve one checkout root per hook directory ([fbed30b](https://github.com/AbysmalBiscuit/devkit/commit/fbed30b8ec7e37cd60cd7ae1b6f294f782a0be87))

## [0.13.4](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.3...v0.13.4) (2026-08-25)


### Bug Fixes

* **plugin:** run bootstrap hook on Windows ([89ddd42](https://github.com/AbysmalBiscuit/devkit/commit/89ddd426f244972db751b24f0b5aa6dfd3eed8ca))

## [0.13.3](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.2...v0.13.3) (2026-08-25)


### Features

* **config:** expand ${VAR} in config values ([093a128](https://github.com/AbysmalBiscuit/devkit/commit/093a1281608acfb2b40d0f4fb0df7f8719a1091b))
* **config:** resolve default paths against their layer ([8f051ee](https://github.com/AbysmalBiscuit/devkit/commit/8f051ee2248c48c27929215037144cb8fe76c34d))
* **dashboard:** read the timeline from the configured tracker ([dc3df67](https://github.com/AbysmalBiscuit/devkit/commit/dc3df67e480a7ff5a8c73ff7309e5d43caf653d0))
* **devkit:** report the GitHub identity in devkit auth ([f0b9ed2](https://github.com/AbysmalBiscuit/devkit/commit/f0b9ed234469fa144804f17955f822d57a456be0))
* **devkit:** report the resolved tracker in doctor ([89efe51](https://github.com/AbysmalBiscuit/devkit/commit/89efe51609eb07d5027891baa75f23ceb78ca5a6))
* **github:** answer a head-branch lookup with a type ([b66fa51](https://github.com/AbysmalBiscuit/devkit/commit/b66fa511963ac21b69247337809eca213f88be59))
* **github:** resolve issue and PR repositories from config ([18f1655](https://github.com/AbysmalBiscuit/devkit/commit/18f16555cb6f03ead918da6ea91dd189ca434b16))
* **issue:** accept a Linear URL and reuse its slug ([e2164b7](https://github.com/AbysmalBiscuit/devkit/commit/e2164b7d3b23f7e6a092075ba9555a517f988fdb))
* **issue:** bind a worktree to its pull request ([8b414f4](https://github.com/AbysmalBiscuit/devkit/commit/8b414f43ab846836b1589180bc51a770c91ab46f))
* **issue:** carry a tagged PR status on every status row ([fe9f27b](https://github.com/AbysmalBiscuit/devkit/commit/fe9f27b3d6a38f774f55a2654e159d9a21748360))
* **issue:** derive the setup slug from the Linear title ([24d54b4](https://github.com/AbysmalBiscuit/devkit/commit/24d54b49ca88590a2d18b968912d8efe7834bfae))
* **issue:** fit a derived setup slug to the branch column ([d2f79d6](https://github.com/AbysmalBiscuit/devkit/commit/d2f79d6f325c70e32489af374bf369419e560343))
* **issue:** keep a pasted PR URL's repository ([cbfc49f](https://github.com/AbysmalBiscuit/devkit/commit/cbfc49f4fe931aa4fcb297f3651babf9fb0e1b62))
* **issue:** report the worktree before running hooks ([b3594ad](https://github.com/AbysmalBiscuit/devkit/commit/b3594ad2409d605bc0581e0bc7a96d365324f41a))
* **issue:** select the tracker from [tracker] kind ([34ef825](https://github.com/AbysmalBiscuit/devkit/commit/34ef82565f85d2593a6686ffed416625f369453a))
* **issue:** show a progress step per worktree hook ([2c9030c](https://github.com/AbysmalBiscuit/devkit/commit/2c9030cbcdd0a312d79b2605e82a7c540d86dfd6))
* **issue:** tie the summary file to the worktree's lifetime ([fe10134](https://github.com/AbysmalBiscuit/devkit/commit/fe10134684d58a265253668f70bd00ac7d05b81e))
* **issue:** write an issue summary file on setup ([f7ce10e](https://github.com/AbysmalBiscuit/devkit/commit/f7ce10e7badc9efbe89229e30dc14d2143570a19))
* **tracker:** add a typed issue state vocabulary ([43f3982](https://github.com/AbysmalBiscuit/devkit/commit/43f3982fad5fc15f87f53293d88428bfd6b2412a))
* **tracker:** add the GitHub Issues adapter ([a2c6222](https://github.com/AbysmalBiscuit/devkit/commit/a2c622237a595665aa40df509b6968918ce3f8b4))
* **tracker:** add the Tracker trait and a no-tracker impl ([0c5c506](https://github.com/AbysmalBiscuit/devkit/commit/0c5c506fa4db22a9088ca814e2797f78894e9c90))
* **tracker:** route setup, checkout and prs through the trait ([da7d270](https://github.com/AbysmalBiscuit/devkit/commit/da7d2707b9248bf48ab3b2e94a82286896ffc041))
* **tracker:** select the GitHub adapter for kind = "github" ([2b2466f](https://github.com/AbysmalBiscuit/devkit/commit/2b2466f7fee6400a91702c1082c4a70430be59a0))


### Bug Fixes

* **config:** absolutize an explicit or env-named config layer ([6edef93](https://github.com/AbysmalBiscuit/devkit/commit/6edef93af680861315e2d6d98806c78f6cd78098))
* **config:** fix path resolution edge cases from review ([2c11b52](https://github.com/AbysmalBiscuit/devkit/commit/2c11b527fe1e7e4e308d10eacfe9a073f26e12ca))
* **config:** make the summary template tracker-neutral ([cdb1f1f](https://github.com/AbysmalBiscuit/devkit/commit/cdb1f1f906ade2d73a93eb15293c8f619ff172f3))
* **config:** resolve `~` against USERPROFILE too ([9588db4](https://github.com/AbysmalBiscuit/devkit/commit/9588db4a74949560309a99729621b526236a7160))
* **dashboard:** scope the github cache to its viewer ([0d3ee08](https://github.com/AbysmalBiscuit/devkit/commit/0d3ee0814160f282876e0a08d45add41beef013c))
* **devkit:** refuse a token passed to auth github ([8b10950](https://github.com/AbysmalBiscuit/devkit/commit/8b109507cc9f5164b67ed536f8e1bdc0cbef4dfe))
* **github:** address task 1 review findings ([fc02960](https://github.com/AbysmalBiscuit/devkit/commit/fc0296089192f81e9afd60740c1733c8c149aa42))
* **github:** drop dead GraphQL error branch, dedupe ambiguity check ([f1ed27e](https://github.com/AbysmalBiscuit/devkit/commit/f1ed27e76de0f5d1cb94e8c175f53e71fe48b8b6))
* **issue:** act on one repository per request ([4b11ae6](https://github.com/AbysmalBiscuit/devkit/commit/4b11ae661cd0c9b83d7bc683a3798ae13352525c))
* **issue:** address hook-progress review findings ([022e133](https://github.com/AbysmalBiscuit/devkit/commit/022e13300434326d57aba6e4204ad0821de02aea))
* **issue:** check out a linked pr from its repo ([3f96b29](https://github.com/AbysmalBiscuit/devkit/commit/3f96b29e1f72b82960ae404a71920e6581960a83))
* **issue:** hold the state gate for a tracker devkit fell back to ([60cef84](https://github.com/AbysmalBiscuit/devkit/commit/60cef8459a8c0a14ec85a8864a8ce3a79481dd9c))
* **issue:** let the live PR answer win over the cache ([b1e8773](https://github.com/AbysmalBiscuit/devkit/commit/b1e8773363faed9db1e4e83e76cd05ce4187014e))
* **issue:** make pr_cell's colour choice actually testable ([04d10d6](https://github.com/AbysmalBiscuit/devkit/commit/04d10d6cfeb684ef20970b058c5ff4f471808a7a))
* **issue:** match issue ids case-insensitively ([9d8870c](https://github.com/AbysmalBiscuit/devkit/commit/9d8870c3dc49e72c268db8ad697c4abeb0eba1c2))
* **issue:** read a Linear URL's id by path position ([6c61e30](https://github.com/AbysmalBiscuit/devkit/commit/6c61e30d11ad9c9e50c6e79f8d354f925f995e6b))
* **issue:** say the state gate is closed, not skipped ([c6a3ce9](https://github.com/AbysmalBiscuit/devkit/commit/c6a3ce9d54837eb741d54d42b4ceca1bde4492d2))
* **issue:** skip the state gate only when there is no tracker ([89d9735](https://github.com/AbysmalBiscuit/devkit/commit/89d97356e399370c84efecd98bf32c0fb87e548b))
* **issue:** sweep legacy summary files regardless of id case ([68699a3](https://github.com/AbysmalBiscuit/devkit/commit/68699a3a3986601ee30516fc97dd42b5d0acafcf))
* **tracker:** accept partial GraphQL NOT_FOUND ([772e40a](https://github.com/AbysmalBiscuit/devkit/commit/772e40a694519e93a2e1c0c96305fc94dac1bdba))
* **tracker:** link a closing issue to its own repo ([ea2fb21](https://github.com/AbysmalBiscuit/devkit/commit/ea2fb21e324668017215b94ae24a6b2c95eceee3))
* **tracker:** match a closing issue repo case-blind ([7960c13](https://github.com/AbysmalBiscuit/devkit/commit/7960c1372be946144110d1b44cb7e8540da4b862))
* **tracker:** match an issue url's repo case-blind ([a65cb04](https://github.com/AbysmalBiscuit/devkit/commit/a65cb0467db8990397b0129ad1bb71477f9be0eb))
* **tracker:** say why a named tracker did not resolve ([987ed5b](https://github.com/AbysmalBiscuit/devkit/commit/987ed5b87ba0779155b23fe82e0f7c58838d7b32))
* **tracker:** tighten declared-gate coverage and drop stale doc claims ([fa1d23d](https://github.com/AbysmalBiscuit/devkit/commit/fa1d23d84772270dfea3a8a50ff82d920f2ffcf6))

## [0.13.2](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.1...v0.13.2) (2026-08-19)


### Features

* **brief:** emit the harness context envelope ([21568f0](https://github.com/AbysmalBiscuit/devkit/commit/21568f0a0d2bbeb0a69c68a5cf1e6b43cc905efe))
* **config:** layer devkit.local.toml over devkit.toml ([fde8f55](https://github.com/AbysmalBiscuit/devkit/commit/fde8f551983bf4418b612de6b1e8c7e0793fb1da))
* **docm:** add forget to release a reference ([462c022](https://github.com/AbysmalBiscuit/devkit/commit/462c0223a79bc251d8bd0bbe6b2aa8fbab4c9dac))
* **docs:** roll up workspace members in the pins table ([46e312f](https://github.com/AbysmalBiscuit/devkit/commit/46e312f0f8cfe25c357533f9d32eeda322fcf77f))
* **hooks:** add after_worktree_create hook ([dfdb214](https://github.com/AbysmalBiscuit/devkit/commit/dfdb214d89479fff4267d59d24a655d81638c578))


### Bug Fixes

* **brief:** stop claiming every pinned version comes from a manifest ([93ef2d6](https://github.com/AbysmalBiscuit/devkit/commit/93ef2d6faff737f4b03abf45fc5b73c86970c5fe))


### Reverts

* drop the stale-reference note from the docs skill ([d7daeb2](https://github.com/AbysmalBiscuit/devkit/commit/d7daeb2bba59e7840fef9bc79ec1357c7980ac10))

## [0.13.1](https://github.com/AbysmalBiscuit/devkit/compare/v0.13.0...v0.13.1) (2026-08-16)


### Features

* **brief:** add --pins-only and --if-changed ([a444f56](https://github.com/AbysmalBiscuit/devkit/commit/a444f56afc0512034f35d5d55c39e3fd482dc233))
* **brief:** add apps and tasks section switches ([a142559](https://github.com/AbysmalBiscuit/devkit/commit/a14255923fb3726e46541e8c84f2ea3af7892bae))
* **brief:** add the library-pins section ([7187be0](https://github.com/AbysmalBiscuit/devkit/commit/7187be0a309932f70741ea74d26a35477c6c0c58))
* **brief:** drop sections with nothing to report ([538a702](https://github.com/AbysmalBiscuit/devkit/commit/538a702149c23b3c2271875f65ae178a74394634))
* **brief:** gate output on [brief] config ([37269b8](https://github.com/AbysmalBiscuit/devkit/commit/37269b84dfe84e681276614c0b0fdd48a29ca75c))
* **brief:** report a config that does not load ([5eebb49](https://github.com/AbysmalBiscuit/devkit/commit/5eebb49f5510b78a7397aceb0b69d89d0c158640))
* **config:** add devkit schema init ([4a1a249](https://github.com/AbysmalBiscuit/devkit/commit/4a1a249b4e6217befcad0ed6cb78dbf150cdca9b))
* **config:** publish a json schema for devkit.toml ([53783f0](https://github.com/AbysmalBiscuit/devkit/commit/53783f00553cba09eaaecb5b26bb610b483517d5))
* **docm:** add list --project ([9c344ea](https://github.com/AbysmalBiscuit/devkit/commit/9c344ea1181db3e08290e3f87045113b65aedabf))
* **docs:** add inspect, evidence, typed undeclared ([1d5c90a](https://github.com/AbysmalBiscuit/devkit/commit/1d5c90a050f79b191507ed78e3a9e643440b7e51))
* **docs:** add pins readout for a checkout ([77b5568](https://github.com/AbysmalBiscuit/devkit/commit/77b556839eacfffb91e6e1d28d98bde7c8ce2169))
* **docs:** render the pins table ([9a1ca41](https://github.com/AbysmalBiscuit/devkit/commit/9a1ca41b50ca1d8678ce07beab66ac0b2712b9e1))
* **hooks:** re-inject pins after a codex compaction ([bc23052](https://github.com/AbysmalBiscuit/devkit/commit/bc23052278e1e31e1bd53b762997812e2771fc8a))
* **hooks:** wire write enforcement on codex ([2f44b48](https://github.com/AbysmalBiscuit/devkit/commit/2f44b4854411c36b23c4f598797ee6f71e840cd6))


### Bug Fixes

* **brief:** clear the watermark on --pins-only ([7e43d28](https://github.com/AbysmalBiscuit/devkit/commit/7e43d28bc63a359fe3da68b0ef1fd943fc1f54c7))
* **brief:** drop [defaults]-optionality, fix fixtures instead ([94c3e40](https://github.com/AbysmalBiscuit/devkit/commit/94c3e402b78eec552ea94aad8c5562e7b951fe7d))
* **brief:** isolate test cache, fix devrun claim ([c0a1368](https://github.com/AbysmalBiscuit/devkit/commit/c0a136858402ea8252f1fcf4f15b94f36c10d124))
* **brief:** stamp the watermark on a full brief ([05c6d83](https://github.com/AbysmalBiscuit/devkit/commit/05c6d83068cff24abc89affadbb97c4418a871d6))
* **ci:** pin the schema file to lf line endings ([39b7c8e](https://github.com/AbysmalBiscuit/devkit/commit/39b7c8e553087b49a580238f91698f1dfa7716f9))
* **config:** let a standalone section stand alone ([0bf8bea](https://github.com/AbysmalBiscuit/devkit/commit/0bf8bea02aec19d20a6db03906f67a66f0c1507e))
* **docs:** anchor pin workspace on the lockfile dir ([2e02b95](https://github.com/AbysmalBiscuit/devkit/commit/2e02b9525b5c82945c1282cf5300b850b16c041a))
* **docs:** measure and shrink the pins render to its budget ([963c2d7](https://github.com/AbysmalBiscuit/devkit/commit/963c2d766474c634e75bb0e63e8c50d6e9bf4a35))


### Performance Improvements

* **docs:** bound pins on lockfile size ([904f88a](https://github.com/AbysmalBiscuit/devkit/commit/904f88a3cd37256f8c96e1bf3b19ab0265c8a295))
* **docs:** parse each lockfile once per selector ([af94d74](https://github.com/AbysmalBiscuit/devkit/commit/af94d74886440613d57ea55a2c1281967940be3b))

## [0.13.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.12.1...v0.13.0) (2026-08-07)


### ⚠ BREAKING CHANGES

* **docs:** require --allow-default-branch

### Features

* **config:** add per-app url template ([fdaf7bf](https://github.com/AbysmalBiscuit/devkit/commit/fdaf7bfa9038d37e0b764bd0f11676f1cf7a5b3c))
* **docs:** add atomic cache locks ([a8dc387](https://github.com/AbysmalBiscuit/devkit/commit/a8dc3872f799804b8c776bcd861854fe06617162))
* **docs:** materialize on add, verify in info ([0c7cd97](https://github.com/AbysmalBiscuit/devkit/commit/0c7cd97120e4ee33bee64de80062743f9556c6ec))
* **docs:** migrate 0.12 caches on first run ([62f663b](https://github.com/AbysmalBiscuit/devkit/commit/62f663ba121ddbbf6827b6ce1d0fd2b44982c080))
* **docs:** require --allow-default-branch ([7604d20](https://github.com/AbysmalBiscuit/devkit/commit/7604d20ffb754c1317e518b94ac19b56dcaf2bb5))
* **docs:** sweep every checkout in doctor ([b19b973](https://github.com/AbysmalBiscuit/devkit/commit/b19b973e402fc2bead8e038c7ffd488307404295))
* **issue:** add --no-notify to review request ([31f7f8d](https://github.com/AbysmalBiscuit/devkit/commit/31f7f8d1c37c2c6ba84daadbec27b42d6b80fa8b))
* **issue:** gate the review --to requirement on config ([eb0dca1](https://github.com/AbysmalBiscuit/devkit/commit/eb0dca1aa0ef422680debabcda1103891d7df109))


### Bug Fixes

* **docm:** keep completions out of the migration ([f6387da](https://github.com/AbysmalBiscuit/devkit/commit/f6387dafd048896891e1aaddcf4c6835aa34fafb))
* **docs:** always error on multi-ecosystem add hit ([da8ad1f](https://github.com/AbysmalBiscuit/devkit/commit/da8ad1fc0203d9ceb4899e87108d24680bfa6d51))
* **docs:** carry resolution provenance out of warnings ([8055718](https://github.com/AbysmalBiscuit/devkit/commit/805571828c8bcba29512c29a9a8b414a1679dfca))
* **docs:** close four gaps in the upgrade pass ([45d2f88](https://github.com/AbysmalBiscuit/devkit/commit/45d2f881b304ea1b938fcbdd268881a52ae6e65e))
* **docs:** close prune registry ABA ([884d5c8](https://github.com/AbysmalBiscuit/devkit/commit/884d5c8b1d91b338acb643a4f10a4ec74ef2eebf))
* **docs:** correct hard-error messages ([563fb92](https://github.com/AbysmalBiscuit/devkit/commit/563fb929143441a9fee6ef771302a084426e7110))
* **docs:** count default checkouts in doctor ([24e15f5](https://github.com/AbysmalBiscuit/devkit/commit/24e15f532789326ec7bd000f8f4ff818aecc84cd))
* **docs:** cover and harden the fail-closed prune expansions ([b08794d](https://github.com/AbysmalBiscuit/devkit/commit/b08794dd59c4a0bcebb18de79cd462eba8be10a9))
* **docs:** drop an unrecoverable migration record ([c64ec3b](https://github.com/AbysmalBiscuit/devkit/commit/c64ec3b24b8d4c53205691a19cd17130b93ee3a5))
* **docs:** encode library cache names ([97a1557](https://github.com/AbysmalBiscuit/devkit/commit/97a15574873c00e58642a110e5825ef8ac75be6d))
* **docs:** fail closed on unreadable prune inputs ([319419a](https://github.com/AbysmalBiscuit/devkit/commit/319419a2e88a18e25b8741a5bc0f91cf978160df))
* **docs:** fail closed on unreadable sidecars ([5b26ef3](https://github.com/AbysmalBiscuit/devkit/commit/5b26ef380b68604319da8e77e3d2e1b74d0ffeec))
* **docs:** guard cache-root prune scans ([d957552](https://github.com/AbysmalBiscuit/devkit/commit/d957552414f9ba92510c4b0a8631e286fed350fb))
* **docs:** key refs by workspace, lock prune ([4d9b7e3](https://github.com/AbysmalBiscuit/devkit/commit/4d9b7e33c44f8fe41437d35393015cb2499a4709))
* **docs:** let prune skip a directory its own name locks out ([0b32cd3](https://github.com/AbysmalBiscuit/devkit/commit/0b32cd39bf56dbf68e545b9c764b0160da436a39))
* **docs:** make prune revisions ABA-safe ([9d0a9d9](https://github.com/AbysmalBiscuit/devkit/commit/9d0a9d9104a116e7a3d12d7075358f0dee9cdac7))
* **docs:** make whole-library deletion recheck absolute ([b02a1a3](https://github.com/AbysmalBiscuit/devkit/commit/b02a1a36559b669e1f72bfca263955f355ad39cc))
* **docs:** name a working reserved-name recovery ([eb90fac](https://github.com/AbysmalBiscuit/devkit/commit/eb90fac5b0392af9e0aa5728ab2e31541cd2df81))
* **docs:** patch a project entry instead of replacing it ([eda8ff0](https://github.com/AbysmalBiscuit/devkit/commit/eda8ff0955229c2bf591a34fcb3cdbc4101beaf9))
* **docs:** preserve live checkout refs ([0b48d72](https://github.com/AbysmalBiscuit/devkit/commit/0b48d726e38a30e258776c779fb34ace2c49c3cd))
* **docs:** prioritize package-specific tags ([eb72305](https://github.com/AbysmalBiscuit/devkit/commit/eb723059def5891c1262f9e11db4fe85262014ce))
* **docs:** qualify the doctor drift row ([1ee1265](https://github.com/AbysmalBiscuit/devkit/commit/1ee1265b6c604fb29715012a524d3eee80e88770))
* **docs:** refuse ambiguous ecosystems and alias rm ([5512cf4](https://github.com/AbysmalBiscuit/devkit/commit/5512cf4fa561ec8cbc66c5b8985db179212594ca))
* **docs:** refuse polyglot ecosystem markers ([d86357f](https://github.com/AbysmalBiscuit/devkit/commit/d86357f81db16c9b1be3fd3ec8eb1226a52bcb03))
* **docs:** reject a prune candidate its lock file cannot fit ([b71410e](https://github.com/AbysmalBiscuit/devkit/commit/b71410e70fc7a83de37e74435c65d0122f577f74))
* **docs:** reject an npm alias install slot ([c6f15ec](https://github.com/AbysmalBiscuit/devkit/commit/c6f15ecbb39ed7f77dec5a4f55c683b30e0b0f8d))
* **docs:** reject non-registry lockfile rows ([3113acb](https://github.com/AbysmalBiscuit/devkit/commit/3113acbf07d92a1dd4115abeb5390aeb4636a2f5))
* **docs:** reject npm remote-tarball installs ([cb8998d](https://github.com/AbysmalBiscuit/devkit/commit/cb8998dcbb030841d649fd941b2a071baafb49c2))
* **docs:** report every unreadable library at once ([05430bf](https://github.com/AbysmalBiscuit/devkit/commit/05430bf6386d99b30d1c808671d362c737be5df2))
* **docs:** reserve the manifest lock stem ([7f95c3c](https://github.com/AbysmalBiscuit/devkit/commit/7f95c3cb099d692171d72afb6e53927dd99e9f1f))
* **docs:** resolve importer graph versions ([4d03143](https://github.com/AbysmalBiscuit/devkit/commit/4d031435ad89314f2afd13c4896b5c17f6b962fd))
* **docs:** restore a project entry verbatim ([5f1628f](https://github.com/AbysmalBiscuit/devkit/commit/5f1628f252cbe9f605a249010aa12b58fa1f60b1))
* **docs:** scope the contention barrier to its lock ([c81ff4a](https://github.com/AbysmalBiscuit/devkit/commit/c81ff4acbedcf809a2f208c69f081ec7b0b1c919))
* **docs:** serialize prune with resolves ([46c8491](https://github.com/AbysmalBiscuit/devkit/commit/46c8491b9a5dea25bedca70c43a1b0711b8ab10d))
* **docs:** skip lock controls in doctor ([8895c75](https://github.com/AbysmalBiscuit/devkit/commit/8895c754d159f35f9cb5b44fb7ae4cc1de4e1e45))
* **docs:** split bun spec at scheme, not last @ ([f586f32](https://github.com/AbysmalBiscuit/devkit/commit/f586f3279a8f7860c59fad3cdb8213fa8acb6449))
* **docs:** unwrap three rejection messages ([30e153f](https://github.com/AbysmalBiscuit/devkit/commit/30e153f314fea20aa991420815fb60d2a6951a06))
* **docs:** validate importer targets ([2049ed9](https://github.com/AbysmalBiscuit/devkit/commit/2049ed901d6ac702d78fde9fa9c7cf7b9c0da4e1))
* **docs:** validate loaded library names ([bb0c3d7](https://github.com/AbysmalBiscuit/devkit/commit/bb0c3d780cdd9f0e437d66c21e47c7938c842f46))
* **docs:** verify ref-named checkouts ([2f688cf](https://github.com/AbysmalBiscuit/devkit/commit/2f688cffc7ae1aa6091b646f3eab40366329ef80))

## [0.12.1](https://github.com/AbysmalBiscuit/devkit/compare/v0.12.0...v0.12.1) (2026-07-22)


### Features

* **ui:** add DEVKIT_HYPERLINKS link override ([79ba23b](https://github.com/AbysmalBiscuit/devkit/commit/79ba23b81133bf8746782a471e9af173d7d10ce8))


### Bug Fixes

* **plugin:** broaden docs skill triggering ([dbbc867](https://github.com/AbysmalBiscuit/devkit/commit/dbbc86722d485a83f5113e4b2ed6284b3931b691))

## [0.12.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.11.0...v0.12.0) (2026-07-20)


### ⚠ BREAKING CHANGES

* **issue:** prs views carry issue_ids as a list

### Features

* **cli:** accept the arg shapes agents guess ([c2d51a2](https://github.com/AbysmalBiscuit/devkit/commit/c2d51a23cc2b37016518d30fee2114a29ff87879))
* **cli:** report --version from every binary ([51f876e](https://github.com/AbysmalBiscuit/devkit/commit/51f876ed9fd6bb9c50eec153450cd45344fd5a15))
* **config:** add [linear] resolve_pr_links option ([6a1b1b3](https://github.com/AbysmalBiscuit/devkit/commit/6a1b1b3924cb4c357e1e98747f03269d8e4e1f0d))
* **devkit:** add brief for session-start orientation ([2c015eb](https://github.com/AbysmalBiscuit/devkit/commit/2c015eb67016744fe8df2705f3e77b8b44c3f147))
* **devkit:** warn on binary/plugin version skew ([2fe1197](https://github.com/AbysmalBiscuit/devkit/commit/2fe1197d5876ae4adea2b1217704299ec3883e94))
* **devrun:** add config tasks listing subcommand ([a9b8062](https://github.com/AbysmalBiscuit/devkit/commit/a9b8062b73d0865fd509439580778dd7476182e2))
* **devrun:** apply task env overlay to up steps ([6a0eee7](https://github.com/AbysmalBiscuit/devkit/commit/6a0eee70caea1ed059213c03880e8f6feb1f8231))
* **devrun:** re-resolve task steps at execution time ([59a84a3](https://github.com/AbysmalBiscuit/devkit/commit/59a84a3a9687f777435370e68ebefc9b952f26d6))
* **hooks:** inject project brief at session start ([0ff14bc](https://github.com/AbysmalBiscuit/devkit/commit/0ff14bc366308a6ebc2d0d07669456434dad7cfd))
* **issue:** add linear issue column to prs review table ([d3114c9](https://github.com/AbysmalBiscuit/devkit/commit/d3114c9a6db84b8fb811fa886226ce9d7ad19099))
* **issue:** config-gate linear pr-link resolution ([1daacb1](https://github.com/AbysmalBiscuit/devkit/commit/1daacb12e3112e408c3c969c12958dd1758a0e53))
* **issue:** union linear-linked issue ids into prs rows ([44e843c](https://github.com/AbysmalBiscuit/devkit/commit/44e843c802ddfea43565f0c30206bb274333894d))
* **linear:** batched pr-url to linked-issues lookup ([cd8cfb7](https://github.com/AbysmalBiscuit/devkit/commit/cd8cfb74e1bf1e10cf47f5e492b9845b980fd008))
* **plugin:** bootstrap binaries at session start ([03913fb](https://github.com/AbysmalBiscuit/devkit/commit/03913fb36ca2a164dc8bb0cba3c002cd117b59c2))
* **ports:** add live_port lookup and holder-scope test ([631bbd2](https://github.com/AbysmalBiscuit/devkit/commit/631bbd227258980021403241bc358be893b98f00))
* **ports:** add require_live field to task config ([b4ff9f5](https://github.com/AbysmalBiscuit/devkit/commit/b4ff9f5d2f21c8936ee7909645799b4a9eb684f0))
* **ports:** gate task execution on live servers ([cd8e8a9](https://github.com/AbysmalBiscuit/devkit/commit/cd8e8a924c8e78a6ef519ccd51c1114a75d5d52a))
* **ports:** user env overrides waive task port refs ([fbed7a6](https://github.com/AbysmalBiscuit/devkit/commit/fbed7a672bfb0e351fc97acac7db445144ff96fd))
* **ports:** validate require_live names and references ([d7d86c3](https://github.com/AbysmalBiscuit/devkit/commit/d7d86c3be5f76d8ef02896eec5e18fea16fb8973))
* **skills:** inject live binary help into using-devkit ([f3ded26](https://github.com/AbysmalBiscuit/devkit/commit/f3ded26b510d291abb86926d4d4b6a75c016fdda))


### Bug Fixes

* **issue:** drop issue-id gate under end --pr-only ([4bcb65b](https://github.com/AbysmalBiscuit/devkit/commit/4bcb65baf795bf6490afc12a9bf5cbc7c09f32db))
* **issue:** page PR searches to avoid GitHub 504 ([141a950](https://github.com/AbysmalBiscuit/devkit/commit/141a950c15ca5ba7ded7498ba801cdd27964a7ad))
* **issue:** resolve prs issue id from title when branch lacks one ([71855f8](https://github.com/AbysmalBiscuit/devkit/commit/71855f8e6c392dd834df544d445e110d0ccf3dfc))
* **ports:** scan merged env for require_live refs ([54a410e](https://github.com/AbysmalBiscuit/devkit/commit/54a410e9c3685c9eb23ac09fc9140c17533a6902))
* **skills:** reword literal preprocessing pattern that aborted skill load ([14dc595](https://github.com/AbysmalBiscuit/devkit/commit/14dc595eacff02b9266d7bb286e253c2204a7682))


### Code Refactoring

* **issue:** prs views carry issue_ids as a list ([2e958b8](https://github.com/AbysmalBiscuit/devkit/commit/2e958b80e20d273a033868ed441da90df0abbefb))

## [0.11.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.10.0...v0.11.0) (2026-07-14)


### ⚠ BREAKING CHANGES

* **docs:** docm checkouts now live under ~/.local/share/devkit/docs; paths printed by earlier docm versions are stale after the first run.
* **ports:** render launch argv and static_env with minijinja

### Features

* **common:** add port-context template render and discovery ([76034ca](https://github.com/AbysmalBiscuit/devkit/commit/76034ca913044b5eb03ef509f27891b0a24d539f))
* **devrun:** add task subcommand for canned oneshots ([4be3c6a](https://github.com/AbysmalBiscuit/devkit/commit/4be3c6aeed7b1008b6a2dae87fe15640d4c4de62))
* **docs:** move the checkout store to the XDG data home ([952305a](https://github.com/AbysmalBiscuit/devkit/commit/952305a5bc8d7b4f8e0933c8764cad88fe73d55b))
* **docs:** resolve js versions from bun.lock ([f108e2c](https://github.com/AbysmalBiscuit/devkit/commit/f108e2cc798126898a4fd9f95e515194d1ee774e))
* **ports:** parse [tasks] config table ([1f73e90](https://github.com/AbysmalBiscuit/devkit/commit/1f73e9034eb5387069a74d7adc8f5ed2cfbfdd35))
* **ports:** render launch argv and static_env with minijinja ([f2f46af](https://github.com/AbysmalBiscuit/devkit/commit/f2f46af004b2e2164eefa9babb8c6522fb78285b))
* **ports:** resolve and exec canned tasks ([51e496c](https://github.com/AbysmalBiscuit/devkit/commit/51e496c285d0e5fcf52296cf0b469827e0527a97))
* **skills:** inject live registry into docs skill ([62c4f68](https://github.com/AbysmalBiscuit/devkit/commit/62c4f6882e87c7aa0f127503377a027ec9fe4ee9))


### Bug Fixes

* **common:** use truthy placeholder in port discovery ([76f4651](https://github.com/AbysmalBiscuit/devkit/commit/76f46517ca05aaa61847746823803a41a5ed8396))
* **devkitd:** make second supervise of live server a no-op ([09a6d21](https://github.com/AbysmalBiscuit/devkit/commit/09a6d21cbaea9204f3add2deb67e0a82cd36b867))
* **devrun:** render up plans in task --dry-run ([513c366](https://github.com/AbysmalBiscuit/devkit/commit/513c366ba988e484b6cf01b7258df374ac3f3522))
* **devrun:** report a live server instead of respawning on up ([e292028](https://github.com/AbysmalBiscuit/devkit/commit/e2920289e44de89246eaa4c3a9c18e856fcafb7e))
* **linear:** reject non-Int issue numbers in graphql queries ([ccee132](https://github.com/AbysmalBiscuit/devkit/commit/ccee132611d661885c9bdd879e737bb4b56a8f3d))

## [0.10.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.9.1...v0.10.0) (2026-07-12)


### Features

* **devkit:** report docs cache health in doctor ([cacf08c](https://github.com/AbysmalBiscuit/devkit/commit/cacf08c855e754338f647fd854e002658c7cd531))
* **docs:** add devkit-docs crate with manifest model ([baa3a5f](https://github.com/AbysmalBiscuit/devkit/commit/baa3a5f6749234aee675eff98ab6a9c5f39f820f))
* **docs:** add docm CLI ([d7993a0](https://github.com/AbysmalBiscuit/devkit/commit/d7993a0f381228388198702661f77699bfdb51dc))
* **docs:** add flock'd reference registry with prune plan ([026c6d0](https://github.com/AbysmalBiscuit/devkit/commit/026c6d0fc53e99317521ce0f5ab56dde1c52837e))
* **docs:** detect checkout docs/src layout ([04f273d](https://github.com/AbysmalBiscuit/devkit/commit/04f273d1460d07a0dcc42c9fc21119a7a33eacb3))
* **docs:** manage bare clones and version worktrees ([e1ab7bb](https://github.com/AbysmalBiscuit/devkit/commit/e1ab7bb2041b73e660b57f72d08c59dfa1abebe2))
* **docs:** parse Cargo, uv, npm, and pnpm lockfiles ([27c51e2](https://github.com/AbysmalBiscuit/devkit/commit/27c51e294531d6ad73cc93513705ba01e35aa7bd))
* **docs:** probe version-to-tag patterns ([e6f6115](https://github.com/AbysmalBiscuit/devkit/commit/e6f61156acb812ddae86e80683b242d36cdc5737))
* **docs:** resolve entries to version-correct checkouts ([722c83e](https://github.com/AbysmalBiscuit/devkit/commit/722c83e61bce875085cc1c4b90b775b75ab6e612))
* **docs:** resolve repo urls from package registries ([74b8f7e](https://github.com/AbysmalBiscuit/devkit/commit/74b8f7e09fc2613ce82f0032c9017d867f6ba3cd))
* **docs:** ship devkit:docs skill and document docm ([d0fb15d](https://github.com/AbysmalBiscuit/devkit/commit/d0fb15df36e5c70afa16a034134cbca9a7898bd3))
* **docs:** write global and project manifest entries ([a25123e](https://github.com/AbysmalBiscuit/devkit/commit/a25123e960f972988703149896fc7fa4be0b5fa8))
* **issue:** add MERGE (unreviewed) to prs action legend ([1c45857](https://github.com/AbysmalBiscuit/devkit/commit/1c45857259c038bfb57ef43bd8334825b402aedb))
* **issue:** distinguish unrequested review from awaiting in prs ([b3f2a01](https://github.com/AbysmalBiscuit/devkit/commit/b3f2a017f07dfe80f4a9305792c4414f1a7c2605))
* **issue:** run issue end removals in parallel background tasks ([b8c67d0](https://github.com/AbysmalBiscuit/devkit/commit/b8c67d0f4fcf8249f5591e563ae4b6f9d14510b9))
* **progress:** make Steps thread-safe and add suspend ([97fe68f](https://github.com/AbysmalBiscuit/devkit/commit/97fe68f4aa78672f7dcdf4dd286e1e9f5d996029))


### Bug Fixes

* **common:** use ? in remote-url slug parse tail ([52d28c8](https://github.com/AbysmalBiscuit/devkit/commit/52d28c8e5f134a2eaa80fa18184bfc1997785db5))
* **docs:** keep prune references when a project's manifest fails to parse ([6478493](https://github.com/AbysmalBiscuit/devkit/commit/6478493494b44ffb5c5ee149fd161d48cfc04a69))
* **docs:** propagate non-NotFound errors reading global manifest ([735bcb9](https://github.com/AbysmalBiscuit/devkit/commit/735bcb9775ff6c7400a09a9ee851d850c5f97311))
* **docs:** scope prune to each project's manifest and reconcile registry writes ([80a689a](https://github.com/AbysmalBiscuit/devkit/commit/80a689a4d64bb3a47c825171f12e180a59c74e17))
* **docs:** skip docm prune confirm prompt when stdin is not a tty ([5ada55d](https://github.com/AbysmalBiscuit/devkit/commit/5ada55d1597968661d5034ccfb4179f3d91c6341))
* **docs:** warn on git default-branch fallback and cover ref attribution ([f9545cb](https://github.com/AbysmalBiscuit/devkit/commit/f9545cbb364e0bf8fab1ddb5881a0e91963d1047))

## [0.9.1](https://github.com/AbysmalBiscuit/devkit/compare/v0.9.0...v0.9.1) (2026-07-07)


### Miscellaneous Chores

* release 0.9.1 ([b08866e](https://github.com/AbysmalBiscuit/devkit/commit/b08866e40a5d55e7f523a24c1c028d4080136c5c))

## [0.9.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.8.0...v0.9.0) (2026-07-07)


### Features

* add --timing / --timing-log to issue and devrun ([dd83435](https://github.com/AbysmalBiscuit/devkit/commit/dd834357484933b1d5758ecc30e60b2edefcdd98))
* **common:** add LiveLines and LiveTable stderr widgets ([510f5e1](https://github.com/AbysmalBiscuit/devkit/commit/510f5e1fb73fb2f1d8af78eee4e57426be9a3692))
* **common:** add minutes tier to step elapsed times ([9b23aed](https://github.com/AbysmalBiscuit/devkit/commit/9b23aeda942017f6a83709979c91ec30dc3c080b))
* **common:** persistent step-log mode for Steps ([6bc4e9e](https://github.com/AbysmalBiscuit/devkit/commit/6bc4e9e6f8749ac3573cdbede45b13290f9f9052))
* **issue:** add Prs::empty and streaming dirty checks ([50dd69d](https://github.com/AbysmalBiscuit/devkit/commit/50dd69d1b67d652351cd215b76bee89bbb1e0f0d))
* **issue:** live-fill the status triage table ([bd7bf5f](https://github.com/AbysmalBiscuit/devkit/commit/bd7bf5fd1135e491b71008a2ca22debd726f4936))
* **issue:** make step logs persistent ([210358b](https://github.com/AbysmalBiscuit/devkit/commit/210358ba50963c9ec7ed695f4c7cc4041ce9b87c))
* **issue:** move the prs refresh spinner below the block ([b6db66d](https://github.com/AbysmalBiscuit/devkit/commit/b6db66d54fcfe45c4a41b1e44553b7cdf8b456cf))
* **issue:** parallel fetches and live table for info ([662271a](https://github.com/AbysmalBiscuit/devkit/commit/662271a3819daa7b7842e9b276a7ff7a07ec391e))
* **issue:** stale-while-revalidate render for prs ([750ab57](https://github.com/AbysmalBiscuit/devkit/commit/750ab57da8289f639e34e6fed82197e41392f879))
* **issue:** store full pr rows in the prs snapshot cache ([3ea4080](https://github.com/AbysmalBiscuit/devkit/commit/3ea4080f50c799a8ca5762eb98e2e7029a4e3479))
* **plugin:** ship per-host marketplace manifests ([cba0f66](https://github.com/AbysmalBiscuit/devkit/commit/cba0f66f6a804d40b31575d91464bd58e69e3ccb))
* **strays:** reap bare-binary launches like chrome ([0acc9d9](https://github.com/AbysmalBiscuit/devkit/commit/0acc9d972272935067810c6a522defd04158b6e7))
* **timing:** add tracing-based IO timing collector ([9cdf495](https://github.com/AbysmalBiscuit/devkit/commit/9cdf495ddff336fbc1ed8d9ab4ec3705dc85036d))
* **timing:** instrument subprocess and http primitives ([0300c60](https://github.com/AbysmalBiscuit/devkit/commit/0300c609048fe72439942bfe20092b4015d5ac4f))


### Bug Fixes

* **common:** colour indicatif bars by stderr, not stdout ([c0ab6e5](https://github.com/AbysmalBiscuit/devkit/commit/c0ab6e5073af031d0255636d632e05af40cd652c))
* **common:** dim stale cells with dim_all ([7c32303](https://github.com/AbysmalBiscuit/devkit/commit/7c32303cf10cc9ffb47f4acdd8f0e354424a6232))
* **common:** key live styling to the stream it draws on ([21918b0](https://github.com/AbysmalBiscuit/devkit/commit/21918b05a2a5aebb8abc8d789271607647f1981a))
* **common:** retire line bars on live block clear ([696a317](https://github.com/AbysmalBiscuit/devkit/commit/696a31739a10335b87a4e2da6dd606c711bc91ef))
* **issue:** collapse status bars into one row ([7130fe1](https://github.com/AbysmalBiscuit/devkit/commit/7130fe1eed0a52ad9b616d6115076f792dd8e42a))
* **issue:** match worktree selector by pr number ([e838353](https://github.com/AbysmalBiscuit/devkit/commit/e8383535d518d5802fbf51f98223b3084db7bdb2))
* **issue:** skip live rendering for info --json ([fcc8d7d](https://github.com/AbysmalBiscuit/devkit/commit/fcc8d7d522da7530c0f425705d9634020951168d))
* **issue:** swap prs stale block for fresh tables in place ([5a17eac](https://github.com/AbysmalBiscuit/devkit/commit/5a17eac60c3185388a41adbf3d70d3a105445c90))
* **ports:** use a real holder dir in the pidless down test ([84b1dab](https://github.com/AbysmalBiscuit/devkit/commit/84b1dabcbbbf4814ec227592b62b0c652b75c7e5))
* **timing:** warn when a subscriber blocks install ([e049f8c](https://github.com/AbysmalBiscuit/devkit/commit/e049f8c1f2e8cb8f91dfbe74e9acd542cb45a34c))
* **worktree:** skip pr-number marker in issue id parse ([09d6fb4](https://github.com/AbysmalBiscuit/devkit/commit/09d6fb441311546660940d5e00ceeebf4d89ca9f))

## [0.8.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.7.0...v0.8.0) (2026-07-01)


### Features

* **common:** add shared progress Steps helper ([eee20a0](https://github.com/AbysmalBiscuit/devkit/commit/eee20a0d23851b52e8eed0dad05882facd422da2))
* **config:** add checkout_worktree_dir template ([ef0bfaf](https://github.com/AbysmalBiscuit/devkit/commit/ef0bfaf1b706e7319cdc97c2aa229ba49361fef8))
* **config:** add defaults.worktree_include ([12704f4](https://github.com/AbysmalBiscuit/devkit/commit/12704f4a376ee9e9d280df9532d8bd30b6c3de17))
* **config:** rename slack template to review_request, add review_finish ([dd3e0be](https://github.com/AbysmalBiscuit/devkit/commit/dd3e0be6859b8d04b22621b0f1dbf6a89ba80816))
* **devkit:** doctor warns about servers outside devrun ([7e29275](https://github.com/AbysmalBiscuit/devkit/commit/7e29275adaea163435beb947ab11ef19fd2afc3a))
* **devkit:** show progress spinners during auth and doctor ([94b5a44](https://github.com/AbysmalBiscuit/devkit/commit/94b5a4470e0e92c727bb942c9acabc58e7c7d700))
* **devrun:** add TTY-gated reap for untracked servers ([f4132cf](https://github.com/AbysmalBiscuit/devkit/commit/f4132cf4e8ccc7c4ad5270d09ff31d828c5f6606))
* **devrun:** show progress spinners during up ([41cb90e](https://github.com/AbysmalBiscuit/devkit/commit/41cb90ed0ff14e0ce21d4e9afb8d0a4811dc0d6f))
* **devrun:** show untracked strays in status ([5c22eee](https://github.com/AbysmalBiscuit/devkit/commit/5c22eeee1149c58227afbc77a64962e91e4cd59c))
* **issue:** add checkout-pr identifier resolution ([f1750e5](https://github.com/AbysmalBiscuit/devkit/commit/f1750e54705655616b8466f52c5d72248de18c60))
* **issue:** add info subcommand ([e44d016](https://github.com/AbysmalBiscuit/devkit/commit/e44d016e40651180ce7eff172b22a242bfde6714))
* **issue:** add network-free gather_local ([c28b37e](https://github.com/AbysmalBiscuit/devkit/commit/c28b37e3859ae4f10a0e5c127873ec2a74f4c41f))
* **issue:** add per-worktree pr cache ([b1b013d](https://github.com/AbysmalBiscuit/devkit/commit/b1b013df96352d8692ee7793c583886f10b3ab38))
* **issue:** add review finish to Slack the PR author ([3e2b025](https://github.com/AbysmalBiscuit/devkit/commit/3e2b0251168187d3fdeec7eaeb175c043cea07ff))
* **issue:** backfill local files on checkout-pr ([54b191d](https://github.com/AbysmalBiscuit/devkit/commit/54b191dca935e70dbe919fdefbdab0f693c75e50))
* **issue:** backfill local files on setup ([2f8baa9](https://github.com/AbysmalBiscuit/devkit/commit/2f8baa9031033907bb87008b42011513895cd97d))
* **issue:** create worktree and check out PR branch ([4054c21](https://github.com/AbysmalBiscuit/devkit/commit/4054c2178803615428107c4a091b610bf923e013))
* **issue:** keep .devkit/ untracked with a self-ignore file ([6ccb6f0](https://github.com/AbysmalBiscuit/devkit/commit/6ccb6f06577add5371cb50bd21afd740f6ed6c40))
* **issue:** rework review request with multi-target --to and --arg ([7b956d4](https://github.com/AbysmalBiscuit/devkit/commit/7b956d407edd96504fc08efabca25132b3af80dd))
* **issue:** run per-app prep on checkout-pr --setup ([4d773cf](https://github.com/AbysmalBiscuit/devkit/commit/4d773cf209cf3344538edee711e3d80f68e2c4ce))
* **issue:** show progress spinners during checkout-pr ([dd68bf8](https://github.com/AbysmalBiscuit/devkit/commit/dd68bf8fc2fbde3df8eab1fdc45bf3884912bb43))
* **issue:** show progress spinners during info and end ([8612877](https://github.com/AbysmalBiscuit/devkit/commit/86128779b79faf1916204cf12bd05820305d528a))
* **issue:** show progress spinners during review request and finish ([786cb9e](https://github.com/AbysmalBiscuit/devkit/commit/786cb9ee696edbadfacbdf6dcccd697a09e44651))
* **issue:** show progress spinners during setup ([b418284](https://github.com/AbysmalBiscuit/devkit/commit/b4182844366362d4e2a291efa6507e6dbf285c69))
* **linear:** resolve issue to PR and look up by number ([febbe84](https://github.com/AbysmalBiscuit/devkit/commit/febbe840fcf49ea7d2de744a4f4e7bb28f6522f3))
* **mcp:** expose read-only ports.strays detection ([9bd7666](https://github.com/AbysmalBiscuit/devkit/commit/9bd766666883d415725ed7dc77e23f5c822a8513))
* **ports:** add strays facade skeleton and data model ([8f99f4a](https://github.com/AbysmalBiscuit/devkit/commit/8f99f4a53861395a7664b62672da8d1c7834c358))
* **ports:** derive dev-server signatures from launch argv ([12bb0e4](https://github.com/AbysmalBiscuit/devkit/commit/12bb0e46805a0968341b8a73121a8d7b2989846e))
* **ports:** merge port and process stray passes ([14e08ab](https://github.com/AbysmalBiscuit/devkit/commit/14e08abc72c292dc8e62c16e020f4a865c986c91))
* **ports:** port-band stray detection pass ([0fa2cf1](https://github.com/AbysmalBiscuit/devkit/commit/0fa2cf1627010e5e2c5562cbf1fd734acc585740))
* **ports:** process-table stray pass with launch-root climb ([2c97bba](https://github.com/AbysmalBiscuit/devkit/commit/2c97bba0804807ab06f8e180e3883c66af6aed82))
* **ports:** real /proc and port-probe seams with kill_tree ([5ba435c](https://github.com/AbysmalBiscuit/devkit/commit/5ba435c470206be7341914a2b375840389a38afc))
* **worktree:** add copy_includes backfill helper ([fe24372](https://github.com/AbysmalBiscuit/devkit/commit/fe2437254610c741cca0256d133ab85884ba942c))


### Bug Fixes

* **devrun:** name stray source in snake_case ([cd25495](https://github.com/AbysmalBiscuit/devkit/commit/cd25495bb3417471c0d5bb3ac2044c27ac9eef06))
* **issue:** blank linear/verdict in cache-only info ([88a4c7c](https://github.com/AbysmalBiscuit/devkit/commit/88a4c7ce09623e59392e3bb2af2fcbb8e1f4bf6b))
* **issue:** clean up orphan worktree on checkout failure ([e70598f](https://github.com/AbysmalBiscuit/devkit/commit/e70598f7d191c8fa333ab14b7cc649b85e49ef80))
* **issue:** include branch and issue record in review finish context ([49e694c](https://github.com/AbysmalBiscuit/devkit/commit/49e694c8d8e794aae25154ec2509d8e66bd81a0e))
* **issue:** judge only latest re-run check attempt ([43a9c08](https://github.com/AbysmalBiscuit/devkit/commit/43a9c0849e4c075c8a143e401f8b8e7acdd1ab98))
* **issue:** report accurate PR review and check status ([9b6f0cd](https://github.com/AbysmalBiscuit/devkit/commit/9b6f0cd20a5a430c15f177493660f155247ebd17))
* **issue:** report on the current dir in info ([5c6972e](https://github.com/AbysmalBiscuit/devkit/commit/5c6972ed8fc9b2b46b92639b0f2947da89b774ab))
* **issue:** tolerate null submittedAt in pr reviews ([00b4a48](https://github.com/AbysmalBiscuit/devkit/commit/00b4a480171f939e488ce6ea83c3ad442c9894e1))
* **ports:** drop generic-only launch signatures ([161d8bf](https://github.com/AbysmalBiscuit/devkit/commit/161d8bf54732306bf256d67b01da59e9a53705fb))
* **ports:** gate port_from_argv to unix ([fcadbba](https://github.com/AbysmalBiscuit/devkit/commit/fcadbbab77d60ae75d2674e1eddfd13a10533cec))


### Performance Improvements

* **devrun:** skip baseline reset when head at ref ([318e71b](https://github.com/AbysmalBiscuit/devkit/commit/318e71b73aa2478b657fa045f2b3bc8d3a0fdd2d))
* **fetch:** gate git fetch on a freshness TTL ([6048324](https://github.com/AbysmalBiscuit/devkit/commit/6048324e41a2704e81acdde15903d0159c598f9f))
* **github:** read PRs over direct HTTP instead of gh ([5df12b4](https://github.com/AbysmalBiscuit/devkit/commit/5df12b40d48c551d402c814023783d5421e4ad40))
* **issue:** cut per-worktree git cost in info and status ([b0abbce](https://github.com/AbysmalBiscuit/devkit/commit/b0abbce4080958952e655a12a62457417e6a83c1))
* **issue:** parallelize dashboard pr fetches ([b011aa3](https://github.com/AbysmalBiscuit/devkit/commit/b011aa375cfedc1e684eabf5d88b5e1de815fa99))
* **issue:** parallelize status gather fan-out ([610ee5b](https://github.com/AbysmalBiscuit/devkit/commit/610ee5ba9a9a2499a8533db9332e41ebf16bb07e))
* **secrets:** cache secrets file per process ([33ebf38](https://github.com/AbysmalBiscuit/devkit/commit/33ebf38125623feda3e70a5710d5fee2ebe755a4))

## [0.7.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.6.0...v0.7.0) (2026-06-24)


### Features

* **common:** add credential secret store ([af5ebce](https://github.com/AbysmalBiscuit/devkit/commit/af5ebcee220dc4a4f8fde5c3c8296d8b5702593c))
* **common:** add credential validators ([49688d6](https://github.com/AbysmalBiscuit/devkit/commit/49688d632e8dc8ca1c7b6fe9f00e00c1c3664829))
* **common:** add minijinja template render helper ([bc10c35](https://github.com/AbysmalBiscuit/devkit/commit/bc10c358f557a7556f12ff3282e3fdc4f449dfdd))
* **config:** add prep_files app field alongside prep_env ([a62f799](https://github.com/AbysmalBiscuit/devkit/commit/a62f79982dd8b17300071fc4674a779976d46e30))
* **devkit:** add devkit binary with auth command ([f103674](https://github.com/AbysmalBiscuit/devkit/commit/f10367469d3a3f7c8008390d44d9d0b4273bf59d))
* **devkit:** add doctor command ([c80c7fa](https://github.com/AbysmalBiscuit/devkit/commit/c80c7faa3f6bb036544bc89c13df31bbc42eb4a6))
* **devkitd:** add DownPorts request and handler ([e1f513f](https://github.com/AbysmalBiscuit/devkit/commit/e1f513fc27e71d717ffc70ae32857898cfa2329c))
* **devrun:** add config apps catalog listing ([d0ea447](https://github.com/AbysmalBiscuit/devkit/commit/d0ea4475eb9257577d13ab75d55432dd35d08478))
* **devrun:** add config show with optional value provenance ([84d87a6](https://github.com/AbysmalBiscuit/devkit/commit/84d87a60ff2d91d77563cba2608622902131309b))
* **devrun:** add launch-time prd guard for doppler launches ([2582439](https://github.com/AbysmalBiscuit/devkit/commit/2582439137a99da4268f72b7159c15621c05a68f))
* **devrun:** cross-worktree down with tty-gated confirmation ([7d4caaa](https://github.com/AbysmalBiscuit/devkit/commit/7d4caaa899161c96f1ce82a75c7b1e55c676c704))
* **devrun:** run launch argv verbatim instead of wrapping in doppler ([7cbf022](https://github.com/AbysmalBiscuit/devkit/commit/7cbf02258db502c98a48a0ac1735fc4b8cc13aef))
* **issue:** ensure .devkit is in the global gitignore ([63fa2a3](https://github.com/AbysmalBiscuit/devkit/commit/63fa2a3c4b98159710747c8943b38f6433476fd6))
* **issue:** persist setup record for review-time context ([294e125](https://github.com/AbysmalBiscuit/devkit/commit/294e12562e39c4db797ba92daae0876a5d09095f))
* **issue:** render branch and worktree dir from templates ([9b81d17](https://github.com/AbysmalBiscuit/devkit/commit/9b81d17f71725e0198ad0211537c5cc7524af43e))
* **issue:** render PR and Slack text from templates ([beee371](https://github.com/AbysmalBiscuit/devkit/commit/beee3713f8a2879561d1065ebb05d94f8f37da10))
* **issue:** template prep-file content at setup ([82ada50](https://github.com/AbysmalBiscuit/devkit/commit/82ada508a7f25b7644a16f981ac79722f839a81e))
* **issue:** write configurable prep_files during setup ([68a10c5](https://github.com/AbysmalBiscuit/devkit/commit/68a10c5cf5addd8147d687a36a67269b91257779))
* **locks:** add explicit-context daemon-aware facade fns ([9aac338](https://github.com/AbysmalBiscuit/devkit/commit/9aac3383bc8394c3e324d14d4359452c3b954720))
* **mcp:** echo the client's initialize protocol version ([7f528a7](https://github.com/AbysmalBiscuit/devkit/commit/7f528a743046bf27d169328d2dcfae9896eca07a))
* **mcp:** route lock actions through the daemon-aware facade ([977153e](https://github.com/AbysmalBiscuit/devkit/commit/977153ed7d6d14f8f9e8527ed1508109af81d1f9))
* **ports:** add deep-merge with per-leaf provenance ([3ef0cd9](https://github.com/AbysmalBiscuit/devkit/commit/3ef0cd90ef9d25ae56c07697d77fb04a54694cea))
* **ports:** add Templates config struct with defaults ([dd3942b](https://github.com/AbysmalBiscuit/devkit/commit/dd3942b1d7ffdc67bca18262c28a7312adee5017))
* **ports:** layer devkit.toml from cwd to root plus home config ([e91d564](https://github.com/AbysmalBiscuit/devkit/commit/e91d5647254f0b730b9b1d1449a87826ba2a0a59))
* **ports:** make config types serializable for config show ([6e33456](https://github.com/AbysmalBiscuit/devkit/commit/6e33456f9e709a64da29b8e98317a95f37d43b46))
* **ports:** resolve layered config through load and expose provenance ([fdc8701](https://github.com/AbysmalBiscuit/devkit/commit/fdc87010ce7239babc1e4e85521340cd8e97e6c1))
* **registry:** add down selection model ([3953ce0](https://github.com/AbysmalBiscuit/devkit/commit/3953ce0ac5b87c45e3bd9a024f9ab0012d0a5733))
* **registry:** release ports by explicit set ([56ac606](https://github.com/AbysmalBiscuit/devkit/commit/56ac606d1ee0708fa1009656e7d69b2b621f61f4))
* **run:** add bring_down_ports facade ([39e9121](https://github.com/AbysmalBiscuit/devkit/commit/39e91216d8a4f2309296c6d3b9dcd1682f8055a9))


### Bug Fixes

* **issue:** gate await re-review on re-request ([5fec5eb](https://github.com/AbysmalBiscuit/devkit/commit/5fec5ebcd827c9ad48e957f221c1bbbe0992a804))
* **issue:** keep changes-requested vote over later comment ([90b96a3](https://github.com/AbysmalBiscuit/devkit/commit/90b96a37184cddfb8e9b6b141438c25940d8e70b))
* **ports:** make doppler_yaml key optional ([23bbb44](https://github.com/AbysmalBiscuit/devkit/commit/23bbb44eab082f023977941b3b000c1c270b1f9e))

## [0.6.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.5.0...v0.6.0) (2026-06-23)


### Features

* **locks:** enable write harness via env and global config ([54cd3f3](https://github.com/AbysmalBiscuit/devkit/commit/54cd3f3b770f81bebacf387b4b6985a350dd9bd8))

## [0.5.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.4.0...v0.5.0) (2026-06-22)


### Features

* **devkitd:** add install-service and systemd autostart routing ([65859d5](https://github.com/AbysmalBiscuit/devkit/commit/65859d5071a14ae5ab15c65e9d606ec7030c1eab))
* **devkitd:** add memory_max_mb config and cap resolution ([79bbb14](https://github.com/AbysmalBiscuit/devkit/commit/79bbb14bceb6f0ad49efb3a10aff68242318b1aa))
* **devkitd:** cage supervised servers in cgroup leaves ([c96ff9b](https://github.com/AbysmalBiscuit/devkit/commit/c96ff9b0a8cc326d7f748175f5d47c751dddb835))
* **sys:** add cgroup-v2 capability and leaf primitives ([cf9bd56](https://github.com/AbysmalBiscuit/devkit/commit/cf9bd56234bcc0655da3b25c350d90bb92eb470a))


### Bug Fixes

* **daemon:** surface systemctl start failure ([ff95ce0](https://github.com/AbysmalBiscuit/devkit/commit/ff95ce0d8aba89d3b3f258b156873d451e2727ef))
* **devkitd:** make cgroup leaf names collision-free ([4c01e4d](https://github.com/AbysmalBiscuit/devkit/commit/4c01e4d1a75f2ebee9ab19b23b2bc53757dac6bb))
* **sys:** check cgroup base is writable ([6cea82b](https://github.com/AbysmalBiscuit/devkit/commit/6cea82b9d1a16f05dd1f651b1a0413c569066846))
* **sys:** delegate memory controller to leaf cgroups ([4f359e5](https://github.com/AbysmalBiscuit/devkit/commit/4f359e5ee08c1743ee38d68793928b9153f775bf))

## [0.4.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.3.0...v0.4.0) (2026-06-22)


### Features

* **devkitd:** add memory_limit_ticks config ([6259286](https://github.com/AbysmalBiscuit/devkit/commit/6259286bc0c90fd963a9bf32bb4deb707e0e8079))
* **devkitd:** add non-recording budget peek ([41e4881](https://github.com/AbysmalBiscuit/devkit/commit/41e4881ddaace8ebf880acc69ce9edc26b6679b2))
* **devkitd:** decide memory-limit restarts ([7370d69](https://github.com/AbysmalBiscuit/devkit/commit/7370d69bac921cab293264db20d63703cdedd78b))
* **devkitd:** restart servers over memory limit ([3e20bd6](https://github.com/AbysmalBiscuit/devkit/commit/3e20bd64355828692d80f7d79fbeb6c596341180))
* **devkitd:** serve write-decide and prefix-release over locks.sock ([1b487df](https://github.com/AbysmalBiscuit/devkit/commit/1b487df511015c62281191bf5df9ab9c2d9efb0c))
* **issue:** add multi-bar Steps progress helper ([116c347](https://github.com/AbysmalBiscuit/devkit/commit/116c347c138f90d7fd1919c50b7464a9ce7210b3))
* **issue:** add step bars and parallel history to dashboard ([ba5b3be](https://github.com/AbysmalBiscuit/devkit/commit/ba5b3bee6f19f7a6ec6dee902ab92f159d670ac5))
* **issue:** extract pr triage into devkit-issue ([27de75a](https://github.com/AbysmalBiscuit/devkit/commit/27de75ae2b2e154a348db1e7e4b4d79b078c1a8d))
* **issue:** extract status gathering into devkit-issue ([fc448a1](https://github.com/AbysmalBiscuit/devkit/commit/fc448a145cd9bb0bc032eb127bb0925e46d35460))
* **issue:** fetch prs and linear workspace in parallel ([f521289](https://github.com/AbysmalBiscuit/devkit/commit/f5212898e5c014ede87f43a25430e6359944ae39))
* **issue:** show parallel step bars for status ([cd56c98](https://github.com/AbysmalBiscuit/devkit/commit/cd56c9801b864bb90e5e98181606f26205cd15ac))
* **linear:** add per-page progress callback to issue history ([86230c4](https://github.com/AbysmalBiscuit/devkit/commit/86230c4496852e09592d035e083f82b778c3990a))
* **lockm:** add hook subcommand enforcing write access ([2bec067](https://github.com/AbysmalBiscuit/devkit/commit/2bec0671d37f600e4a460939fc8a41c1aaf406a6))
* **locks:** add ancestor-aware write decision and prefix release ([4ff8eeb](https://github.com/AbysmalBiscuit/devkit/commit/4ff8eeb75331ef19873a791a30e124030323c528))
* **locks:** add decide_write and release_prefix facade ([a93de57](https://github.com/AbysmalBiscuit/devkit/commit/a93de57609f6d94b71fcafc07f25b6ba9aca33f9))
* **locks:** add holder ancestor-or-self predicate ([817fb90](https://github.com/AbysmalBiscuit/devkit/commit/817fb90a01c22ba76d8b42dfcd4fcca017dff8c1))
* **locks:** add hook payload parsing and activation gate ([3bc4c3b](https://github.com/AbysmalBiscuit/devkit/commit/3bc4c3ba8a3c635283c46ea98e1aa8a6693a68cf))
* **locks:** add write-decide and prefix-release store ops ([0767390](https://github.com/AbysmalBiscuit/devkit/commit/0767390ae70d103ce8ff5fc7440a4951a836e42c))
* **mcp:** add devrun.down and devrun.logs actions ([24a0864](https://github.com/AbysmalBiscuit/devkit/commit/24a0864067adbb16f0344d1bd7cb0c03a5e2d499))
* **mcp:** add devrun.status action ([8f8d39c](https://github.com/AbysmalBiscuit/devkit/commit/8f8d39c3b692172e1ba2b1c7421af1ae857b75a8))
* **mcp:** add issue.status and issue.prs actions ([1ace104](https://github.com/AbysmalBiscuit/devkit/commit/1ace104c45edef29ee030e6ce9126fb64c741cc0))
* **mcp:** add non-blocking devrun.up action ([ca99e4f](https://github.com/AbysmalBiscuit/devkit/commit/ca99e4f1e789f98478ee2262fee9d7a6763bb3cc))
* **plugin:** wire write-harness hooks and dogfood opt-in ([65ec462](https://github.com/AbysmalBiscuit/devkit/commit/65ec4625d73dc93602b42cf835a7cbb353f320cd))


### Bug Fixes

* **issue:** restore prs workspace spinner step ([0aaaef8](https://github.com/AbysmalBiscuit/devkit/commit/0aaaef80e34693712d29194e1d8ec4ac9fb7133b))
* **locks:** ignore hook payloads without a session id ([dd8ebae](https://github.com/AbysmalBiscuit/devkit/commit/dd8ebae1c9058bef1a1b5e54054f83b64ee06822))
* **locks:** pin harness write locks to ttl, not pid ([89ed986](https://github.com/AbysmalBiscuit/devkit/commit/89ed9862e6e7f7b6a6832f68f5dbb4cba8cde217))
* **locks:** release holder locks across all roots ([d610b06](https://github.com/AbysmalBiscuit/devkit/commit/d610b063267a3049368791a381aa155cee1c7ec3))
* **plugin:** stop double-loading hooks/hooks.json ([16d4ba2](https://github.com/AbysmalBiscuit/devkit/commit/16d4ba287e92b1b15504aa7d1e4a15482f92cf2b))
* **registry:** detect listeners via tcp connect ([3a2ac96](https://github.com/AbysmalBiscuit/devkit/commit/3a2ac96eed0baf897ca2541da2f4d4859de07f29))


### Performance Improvements

* **issue:** fetch all prs in one graphql request ([a14b56e](https://github.com/AbysmalBiscuit/devkit/commit/a14b56ea19223e6ca5e3afba9ff7d6d510b79e46))

## [0.3.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.2.0...v0.3.0) (2026-06-22)


### Features

* add claude code plugin manifest ([efc3432](https://github.com/AbysmalBiscuit/devkit/commit/efc3432926ba5508769f05384650cacfa53e2328))
* add codex and cursor skill plugins ([8a78c0f](https://github.com/AbysmalBiscuit/devkit/commit/8a78c0f5b41cb596ad341d256a48e5b3fcd9fa2d))
* **config:** add health-probe daemon knobs ([2da382e](https://github.com/AbysmalBiscuit/devkit/commit/2da382ecad441c8b49ea283398da0e9974f70226))
* **devkitd:** restart hung servers via a health probe ([514af3b](https://github.com/AbysmalBiscuit/devkit/commit/514af3ba18e9d66c20b1870de648ed1902d33a4f))
* **devkitd:** serve the lock registry from memory over locks.sock ([c8965ab](https://github.com/AbysmalBiscuit/devkit/commit/c8965ab8ccdb2105a4ef7855c7d8bb22216f9968))
* **devkitd:** track health-probe state per supervised child ([10ac971](https://github.com/AbysmalBiscuit/devkit/commit/10ac971e638e4c4374ffc985cd44ba3d87f4d56b))
* **issue:** improve status/PR table rendering ([f1eb89c](https://github.com/AbysmalBiscuit/devkit/commit/f1eb89ceb3306e6e83e93c22338deeee342ca11d))
* **locks:** add lock daemon proto and locks.sock client ([a0e8588](https://github.com/AbysmalBiscuit/devkit/commit/a0e8588270c0fbcbf02c8573911b107fc4ded7f1))
* **locks:** add MemoryStore write-through driver and startup load ([044a036](https://github.com/AbysmalBiscuit/devkit/commit/044a0362d25b2c48df64f132b62137ea11c23507))
* **locks:** add Store seam, devkitd.lock gate, and generic *_with ops ([ae25513](https://github.com/AbysmalBiscuit/devkit/commit/ae25513f29e3223ecef27cf5a1baed8b506c1461))
* **locks:** route the facade through the daemon when one is up ([d7e25f4](https://github.com/AbysmalBiscuit/devkit/commit/d7e25f41b14687c86043f3007960afe4ac3cacc6))
* **mcp:** add action registry, describe/call, and ports actions ([4423dd3](https://github.com/AbysmalBiscuit/devkit/commit/4423dd316d1e07d8a2c39b36e21e8c5e7212ff13))
* **mcp:** add file-lock actions ([4c7f5b1](https://github.com/AbysmalBiscuit/devkit/commit/4c7f5b15bb46a9b390457bf2ce333e27d45b6e0c))
* **mcp:** handle initialize and tools/list ([c13b2f1](https://github.com/AbysmalBiscuit/devkit/commit/c13b2f128aecbfa211295be47554ee2fef2e1163))
* **mcp:** register server for Codex and Cursor ([5879a99](https://github.com/AbysmalBiscuit/devkit/commit/5879a99efe8eb4a9f82a6f273503db39fcba6db2))
* **mcp:** scaffold devkit-mcp crate with stdio json-rpc loop ([a7026be](https://github.com/AbysmalBiscuit/devkit/commit/a7026bec7aab0851bc1e06b7f2b8af5a51afa327))


### Bug Fixes

* **devkitd:** make supervisor table authoritative for restarts ([6d0b183](https://github.com/AbysmalBiscuit/devkit/commit/6d0b183882ef5d59b4610cf2e39ed3e172927acc))
* **locks:** replace stray NUL byte in test comment with its escape text ([371ac17](https://github.com/AbysmalBiscuit/devkit/commit/371ac17b61c42f2e6ee168debcbe99b030fc3d6a))
* **mcp:** register server in plugin manifest, add acquire-conflict test ([8a4f74c](https://github.com/AbysmalBiscuit/devkit/commit/8a4f74c2328b629a0b03518436961e1c21c83f42))
* **mcp:** use project root as the ports holder for liveness ([53f6620](https://github.com/AbysmalBiscuit/devkit/commit/53f66204bc0a09aac3b0c20c1007b6642b6931a0))

## [0.2.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.1.0...v0.2.0) (2026-06-21)


### Features

* **common:** add slack chat.postMessage poster ([003aef1](https://github.com/AbysmalBiscuit/devkit/commit/003aef1e9a948c953b54309c53898a91055f85e2))
* **common:** git/gh subprocess wrappers with stderr-aware errors ([c3ac32c](https://github.com/AbysmalBiscuit/devkit/commit/c3ac32cfacab3a9c08b390761ef1e4a848088585))
* **common:** Linear assigned-issue history + viewer origin queries ([7566ca3](https://github.com/AbysmalBiscuit/devkit/commit/7566ca3b3ac49435b9a3af83565cb4f6e5bc8698))
* **common:** state/cache/log path helpers ([e3d937a](https://github.com/AbysmalBiscuit/devkit/commit/e3d937a9e231446b13a1ae0ab326776afdb79b22))
* **common:** table/link helpers + batched Linear Done-gate client ([a83f428](https://github.com/AbysmalBiscuit/devkit/commit/a83f428bb348927692d994e1438106689a7f49c3))
* **common:** worktree discovery + issue-id parsing ([4fd928d](https://github.com/AbysmalBiscuit/devkit/commit/4fd928d3900cfeb0dd03aadbdd8b3e6876457e3a))
* **config:** add [daemon] section with serde defaults ([fe27be9](https://github.com/AbysmalBiscuit/devkit/commit/fe27be956cf2d9a419c1c421d259c7a5e477dc35))
* **config:** add [people] aliases and defaults.pr_base ([85bb255](https://github.com/AbysmalBiscuit/devkit/commit/85bb25504fae7ecca4ed1d2e9f0bf43a364fd540))
* **config:** drive api/app conventions from config instead of hardcoding ([3cc64ac](https://github.com/AbysmalBiscuit/devkit/commit/3cc64ac515f23fed0f8817521d4d4672f6676096))
* **daemon:** client connect/handshake/autostart with flock fallback seam ([fbd3b08](https://github.com/AbysmalBiscuit/devkit/commit/fbd3b0822fb135092a95fe810a2f63ffb8ef492c))
* **daemon:** IPC protocol types and JSON-line framing ([8527cba](https://github.com/AbysmalBiscuit/devkit/commit/8527cba0ef9157ee680ef2b7d12d3f08c2a8b07a))
* **daemon:** unify transport on interprocess local sockets ([d2b72cf](https://github.com/AbysmalBiscuit/devkit/commit/d2b72cf52bd5aa40e9645ed095cfddd090a5df43))
* **devrun:** baseline worktree A/B with guarded hard-reset ([a23fe1d](https://github.com/AbysmalBiscuit/devkit/commit/a23fe1d4c9e0b8062643b09b9dd181ab82b7c3b4))
* **devrun:** detached spawn, readiness poll, SIGTERM, log tail ([9f6799b](https://github.com/AbysmalBiscuit/devkit/commit/9f6799b2cdbad7be719f18b334b90852e08d253c))
* **devrun:** doppler prefix + env layering + api-url wiring ([0c0dd39](https://github.com/AbysmalBiscuit/devkit/commit/0c0dd3943d3ca6ca5d46bb329d9f911b4dba6585))
* **devrun:** up --supervise and daemon-aware down; surface daemon supervise errors ([dac0a36](https://github.com/AbysmalBiscuit/devkit/commit/dac0a3660bb01194cf11b85cdb9657fbfde212da))
* **devrun:** up/down/status/logs with dry-run and app auto-resolution ([1f481b0](https://github.com/AbysmalBiscuit/devkit/commit/1f481b0a9ff6a66ba9fc3c25d458288256e481b1))
* example config, README, install instructions ([6a0d6b7](https://github.com/AbysmalBiscuit/devkit/commit/6a0d6b7d2d188ec729564d48d8beb948fdc97343))
* **issue-end:** Rust rewrite (gh + Linear gate + Rust cleanup) ([b5f5722](https://github.com/AbysmalBiscuit/devkit/commit/b5f5722f2efbbabad1e9be6c9e413382a1ce4174))
* **issue-prep:** mechanical worktree+env+port reservation, JSON output ([b6655ab](https://github.com/AbysmalBiscuit/devkit/commit/b6655abb51e7fe29b9decdc0eae24beffa0a5f50))
* **issue:** add review subcommand (push, PR, reviewer, slack) ([fc06961](https://github.com/AbysmalBiscuit/devkit/commit/fc06961ebe4117f48d837dc04c778d62964be738))
* **issue:** assemble dashboard issue/PR/commit timelines ([2e32992](https://github.com/AbysmalBiscuit/devkit/commit/2e32992ed8ebf6b7e7cf350ef765f0b10590c530))
* **issue:** cache the dashboard timeline fetches ([5c3c819](https://github.com/AbysmalBiscuit/devkit/commit/5c3c819ea44b892118cba8e7bce28b36c19ca798))
* **issue:** config-driven setup commands; drop .env symlink ([1a9d728](https://github.com/AbysmalBiscuit/devkit/commit/1a9d728aa1d737cb4b5deaa8ca2d33de56e92570))
* **issue:** dashboard at-a-glance view (triage + PR tables) ([9042f78](https://github.com/AbysmalBiscuit/devkit/commit/9042f7815eaabbea50343f724476697aa7ef8f0a))
* **issue:** extract shared worktree-triage core ([aced166](https://github.com/AbysmalBiscuit/devkit/commit/aced16631b6e419ad3071638ba28663be3e18ce9))
* **issue:** live dashboard data fetch (Linear/gh/git) ([724d97c](https://github.com/AbysmalBiscuit/devkit/commit/724d97c14f400c35000fa5052a11a27004429f54))
* **issue:** port issue-end clean to issue end ([c813e05](https://github.com/AbysmalBiscuit/devkit/commit/c813e055f5f1156114bc4b3ac33c7ac784386330))
* **issue:** port issue-end status to issue status ([721973f](https://github.com/AbysmalBiscuit/devkit/commit/721973fcfac39cecfbde56c1a81e3c407ffb2145))
* **issue:** port issue-prep to issue setup ([7739ae0](https://github.com/AbysmalBiscuit/devkit/commit/7739ae09282aa08c9e7d38afc638b442763b04aa))
* **issue:** port pr-status to issue prs ([a9050f0](https://github.com/AbysmalBiscuit/devkit/commit/a9050f01f2bbef9889e2424973f10f9372689548))
* **issue:** pure date bucketing and issue state replay ([bffe840](https://github.com/AbysmalBiscuit/devkit/commit/bffe8400917151a9f5ec2ec3869497da2cb33a70))
* **issue:** scaffold consolidated issue crate ([fbf084e](https://github.com/AbysmalBiscuit/devkit/commit/fbf084e95c1b1f1d3a12f0f0895231307f7ec827))
* **issue:** terminal bar and line chart rendering ([a7f4a60](https://github.com/AbysmalBiscuit/devkit/commit/a7f4a6080fb2dbd7505997bcccd803e9ee8ba4e8))
* **locks:** acquire/release/check/prune operations ([e33bd9d](https://github.com/AbysmalBiscuit/devkit/commit/e33bd9d576809c670262c7394abc72665a135985))
* **locks:** flock-guarded JSON lock store with salvage ([7643b6c](https://github.com/AbysmalBiscuit/devkit/commit/7643b6ce81e90c84e176e11e83bf78dce80dec1d))
* **locks:** lock CLI binary and startup state migration ([d45d723](https://github.com/AbysmalBiscuit/devkit/commit/d45d72319249a12036647f4fcb8810193adb867b))
* **locks:** lock entry model and path-overlap detection ([c68ab96](https://github.com/AbysmalBiscuit/devkit/commit/c68ab96f2607338b7247cc6571a968be2e1d19b6))
* **locks:** root detection, path normalization, and public ops ([9319a0d](https://github.com/AbysmalBiscuit/devkit/commit/9319a0d854d9ed7e23f0cd93306f8d1b6697e462))
* **locks:** scaffold devkit-locks crate ([35c6d16](https://github.com/AbysmalBiscuit/devkit/commit/35c6d16a11256cfd2a92abe0f628642848e7ead1))
* **locks:** session identity precedence and anchor-pid policy ([47be011](https://github.com/AbysmalBiscuit/devkit/commit/47be01127857565b01bbdb86b9d9705b3d7684f2))
* native Windows build for paths, devrun logs, and tests ([830f4cb](https://github.com/AbysmalBiscuit/devkit/commit/830f4cb37c82405d97cfef7a2034b866a8528d20))
* one-command install via `cargo install --path .` + shell completions ([a311b3e](https://github.com/AbysmalBiscuit/devkit/commit/a311b3e65f5e93b362570ca46c9ac84caabc6c20))
* **paths:** add daemon socket/lock/log paths ([66b3567](https://github.com/AbysmalBiscuit/devkit/commit/66b35673d54b9f9e157fe94f0e5bbf0247b212b2))
* **paths:** move state home to XDG ~/.local/state/devkit with legacy fallback ([1b19e5f](https://github.com/AbysmalBiscuit/devkit/commit/1b19e5f3b274ef9eae4982bc683a776a69146770))
* **portd:** daemon skeleton — single-instance lock, socket, idle-exit ([a6ad9d0](https://github.com/AbysmalBiscuit/devkit/commit/a6ad9d08939723556155200bb42e2ec7720e53b8))
* **portd:** request dispatch, supervision thread, restart, adoption, down coordination ([cecb3e4](https://github.com/AbysmalBiscuit/devkit/commit/cecb3e4184f424f27bd0ab77cf1ad5e2d936fd17))
* **portd:** serve the port registry from authoritative memory ([a43b926](https://github.com/AbysmalBiscuit/devkit/commit/a43b92630626a7ddb7a16eb8ea1071375f927e08))
* **portd:** supervisor table — reap, crash-loop budget, memory tracking, adoption ([f3b02ed](https://github.com/AbysmalBiscuit/devkit/commit/f3b02edfc0f5fae96300975e82df7f4ff8d9eb06))
* **portman:** status/release/prune CLI over the registry ([f9f67f6](https://github.com/AbysmalBiscuit/devkit/commit/f9f67f67635bfa2ab786528e10857312cb4f12db))
* **ports:** app catalog merging config with doppler.yaml ([b6427d7](https://github.com/AbysmalBiscuit/devkit/commit/b6427d7d0f31e3101b7f786afb362e426adcc0a5))
* **ports:** devkit.toml config + doppler.yaml parsing (prd denylist) ([7879893](https://github.com/AbysmalBiscuit/devkit/commit/7879893877fecca61bf3156e4e19d11f723d182e))
* **ports:** registry alloc/release/prune (idempotent reservations) ([8ee1b7e](https://github.com/AbysmalBiscuit/devkit/commit/8ee1b7ee9b97c5f2ca288800b055078e5bf9a834))
* **ports:** registry liveness helpers (listening/pid/holder) ([db02eac](https://github.com/AbysmalBiscuit/devkit/commit/db02eac75db9910d136fcc2237dc647fe37632c4))
* **ports:** registry types, RAII flock, atomic load/save ([e867ddd](https://github.com/AbysmalBiscuit/devkit/commit/e867ddd3d6b7f89c5e4225747a1940826813dc1c))
* **ports:** shared config→catalog loader; wire portman alloc ([8d839ed](https://github.com/AbysmalBiscuit/devkit/commit/8d839ed29459c825e68db9d8ac9c331e4e822a5e))
* **pr-status:** Rust rewrite with before→after diff cache ([ee6926b](https://github.com/AbysmalBiscuit/devkit/commit/ee6926bec384f48d0469b6e29c66ae25059a6b12))
* **registry:** MemoryStore driver with write-through commit point ([f1a6bdf](https://github.com/AbysmalBiscuit/devkit/commit/f1a6bdf059c3b91d5a4b2534d833393073c6cf0a))
* **registry:** route facade through daemon when up, flock fallback ([0e42918](https://github.com/AbysmalBiscuit/devkit/commit/0e42918303e23bd4fdd93148d94e0b52b4237c5f))
* **store:** expose load/save for lock-free owners ([f0c9fb2](https://github.com/AbysmalBiscuit/devkit/commit/f0c9fb2a52615a3d056138239b684be5daae7532))
* **sys:** add Windows backend via windows-sys ([163a948](https://github.com/AbysmalBiscuit/devkit/commit/163a948e9882d0bc5dff04d6315edc57f244ce69))
* **sys:** parent-pid and controlling-tty behind the boundary ([fa7fccf](https://github.com/AbysmalBiscuit/devkit/commit/fa7fccfd6a1a3d0a6f2b0a7211eaa428ac44fcbe))


### Bug Fixes

* **issue:** harden review URL parsing, Linear pagination, and dashboard repo discovery ([7f90ada](https://github.com/AbysmalBiscuit/devkit/commit/7f90adaaf035b63693fada810495e53050211043))
* **issue:** keep dashboard rendering when the PR panel fails ([51bd206](https://github.com/AbysmalBiscuit/devkit/commit/51bd20670088a2c42a1f1b3bc6c7b0f224ed0af7))
* **portd:** load the registry before binding the socket ([47d5bce](https://github.com/AbysmalBiscuit/devkit/commit/47d5bce3d8d3c3e95a080315e9f9010e7e844233))
* **ports:** record_pid upserts + down stops un-pruned entries; grace &gt; readiness timeout ([be0ceb1](https://github.com/AbysmalBiscuit/devkit/commit/be0ceb17e9f6951fb58f9d84ec8c66e60038a5a8))
* **ports:** skip apps with unresolvable path instead of failing the catalog ([6949db4](https://github.com/AbysmalBiscuit/devkit/commit/6949db4a2065f95c0c31b4898f5a4265a86caef1))
* **registry:** make the portd.lock gate unconditional and leak-proof ([6319432](https://github.com/AbysmalBiscuit/devkit/commit/6319432e3442cd4358bae4f1dd9a834507145e05))
* **release:** give root package a concrete version for release-please ([e9913e6](https://github.com/AbysmalBiscuit/devkit/commit/e9913e69c7ea824d62d91429f6530d5afee38c7d))
* **release:** pin member crate versions for release-please ([8415ebb](https://github.com/AbysmalBiscuit/devkit/commit/8415ebb65163630ad827619d7361b67d190ec7ee))
* **sys:** compute tree_rss_bytes on macOS via ps ([6f1bec8](https://github.com/AbysmalBiscuit/devkit/commit/6f1bec8b03eb50302fefa9ba6e37b7e2adb3df14))


### Performance Improvements

* add release profile (thin LTO, codegen-units=1, panic=abort, strip) ([ec0f66f](https://github.com/AbysmalBiscuit/devkit/commit/ec0f66fd3f98c65167d689744de6d91c1bc38edb))
* **pr-status:** parallelize independent gh round-trips ([174dcf6](https://github.com/AbysmalBiscuit/devkit/commit/174dcf6337b23a5e2e0416a761b35b6a5b281b4b))
